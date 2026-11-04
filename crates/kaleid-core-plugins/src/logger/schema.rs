//! Final output-boundary schema. Producers must still choose safe IDs and static codes.
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use kaleid_kernel::{DiagnosticRecord, Severity};
use serde::Serialize;
use serde_json::Value;

pub(super) const ID_FIELDS: &[&str] = &[
    "plugin_id",
    "root_id",
    "version_id",
    "game_version",
    "loader_version",
    "project_id",
    "file_id",
    "source_id",
];
const CODE_FIELDS: &[&str] = &[
    "command",
    "operation",
    "outcome",
    "status",
    "previous_status",
    "phase",
    "step",
    "loader",
    "category",
    "reason",
    "source",
    "previous_source",
    "policy",
    "retry_class",
];
const NUMBER_FIELDS: &[&str] = &[
    "duration_ms",
    "elapsed_ms",
    "runtime_ms",
    "runtime_duration_ms",
    "pid",
    "exit_code",
    "http_status",
    "retry_attempt",
    "attempt",
    "retry_budget",
    "retry_after_ms",
    "pass",
    "done_bytes",
    "total_bytes",
    "files_done",
    "files_total",
    "repaired_files",
    "repaired_bytes",
    "retry_count",
    "fallback_count",
    "integrity_failures",
    "repetitions",
    "dropped_records",
    "count",
    "total",
    "failed",
    "throttle_count",
    "source_fallback_count",
    "range_fallback_count",
    "integrity_failure_count",
    "internal_error_count",
    "warnings_suppressed",
];
const BOOL_FIELDS: &[&str] = &[
    "cancelled",
    "crashed",
    "killed",
    "reached_running",
    "overwrite",
    "skipped",
    "enabled_preserved",
];

#[derive(Serialize)]
pub(super) struct SafeRecord {
    pub timestamp_at_emission_ms: u64,
    pub severity: Severity,
    pub component: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_id: Option<String>,
    fields: BTreeMap<String, Value>,
}

fn bounded(input: &str, chars: usize) -> String {
    input.chars().take(chars).collect()
}

fn code(input: &str) -> Option<String> {
    let value = bounded(input, 96);
    (!value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
        && !pathlike(&value))
    .then_some(value)
}

fn pathlike(value: &str) -> bool {
    value.contains(['/', '\\', '@', '?', '#', '=', '%'])
        || (value.len() >= 2
            && value.as_bytes()[1] == b':'
            && value.as_bytes()[0].is_ascii_alphabetic())
}

fn identifier(input: &str) -> Option<String> {
    let value = bounded(input, 128);
    (!value.is_empty()
        && !pathlike(&value)
        && !value.contains([':', '"', '\'', '<', '>', '{', '}', '[', ']', '&']))
    .then_some(value)
}

// Worst-case JSON encoding cost is accounted before serialization, including control escapes.
fn string_cost(value: &str) -> usize {
    2 + value
        .chars()
        .map(|c| match c {
            '"' | '\\' => 2,
            c if c <= '\u{1f}' => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}

fn take_string(value: Option<String>, budget: &mut usize) -> Option<String> {
    let value = value?;
    let cost = string_cost(&value) + 32;
    if cost > *budget {
        return None;
    }
    *budget -= cost;
    Some(value)
}

pub(super) fn sanitize(record: &DiagnosticRecord) -> SafeRecord {
    // Leaves >1KiB for fixed JSON structure/run ID/sequence, even worst-case u64 values.
    let mut budget = 6500usize;
    let component = code(&record.component).unwrap_or_else(|| "unknown".into());
    let event = code(&record.event).unwrap_or_else(|| "unknown".into());
    budget = budget.saturating_sub(string_cost(&component) + string_cost(&event));
    let mut safe = SafeRecord {
        timestamp_at_emission_ms: record.timestamp_at_emission_ms,
        severity: record.level,
        component,
        event,
        operation_id: take_string(
            record.operation_id.as_deref().and_then(identifier),
            &mut budget,
        ),
        session_id: take_string(
            record.session_id.as_deref().and_then(identifier),
            &mut budget,
        ),
        job_id: take_string(record.job_id.as_deref().and_then(identifier), &mut budget),
        batch_id: take_string(record.batch_id.as_deref().and_then(identifier), &mut budget),
        fields: BTreeMap::new(),
    };
    // Look up finite known keys instead of walking/cloning arbitrary untrusted payloads.
    for (keys, kind) in [
        (ID_FIELDS, 0),
        (CODE_FIELDS, 1),
        (&["host", "previous_host"][..], 2),
    ] {
        for &key in keys {
            let Some(input) = record.fields.get(key).and_then(Value::as_str) else {
                continue;
            };
            let value = match kind {
                0 => identifier(input),
                1 => code(input),
                _ => diagnostic_host(input).filter(|host| host == &input.to_ascii_lowercase()),
            };
            if let Some(value) = take_string(value, &mut budget) {
                safe.fields.insert(key.into(), Value::String(value));
            }
        }
    }
    for &key in NUMBER_FIELDS {
        let Some(value) = record.fields.get(key) else {
            continue;
        };
        if (value.is_u64() || value.is_i64()) && budget >= key.len() + 24 {
            budget -= key.len() + 24;
            safe.fields.insert(key.into(), value.clone());
        }
    }
    for &key in BOOL_FIELDS {
        let Some(value) = record.fields.get(key).filter(|v| v.is_boolean()) else {
            continue;
        };
        if budget >= key.len() + 8 {
            budget -= key.len() + 8;
            safe.fields.insert(key.into(), value.clone());
        }
    }
    safe
}

/// Extract a validated lower-case HTTP(S) host[:port], never URL credentials or payloads.
/// Already host-only input is accepted; bare paths, queries, non-HTTP schemes are rejected.
pub fn diagnostic_host(input: &str) -> Option<String> {
    if input.len() > 8192 || input.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let authority = if let Some((scheme, rest)) = input.split_once("://") {
        if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
            return None;
        }
        rest.split(['/', '?', '#']).next()?.rsplit('@').next()?
    } else {
        if input.contains(['/', '?', '#', '@', '\\']) {
            return None;
        }
        input
    };
    if authority.is_empty() || authority.len() > 260 {
        return None;
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']')?;
        authority.get(1..end)?.parse::<Ipv6Addr>().ok()?;
        let tail = &authority[end + 1..];
        (
            &authority[..=end],
            if tail.is_empty() {
                None
            } else {
                Some(tail.strip_prefix(':')?)
            },
        )
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        if host.is_empty() || host.len() > 253 {
            return None;
        }
        if host.parse::<Ipv4Addr>().is_err()
            && !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return None;
        }
        (host, port)
    };
    if let Some(port) = port {
        if port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
            return None;
        }
    }
    Some(match port {
        Some(port) => format!("{}:{port}", host.to_ascii_lowercase()),
        None => host.to_ascii_lowercase(),
    })
}

#[derive(Serialize)]
struct Line<'a> {
    run_id: &'a str,
    sequence: u64,
    #[serde(flatten)]
    record: &'a SafeRecord,
}

pub(super) fn encode(record: &SafeRecord, run_id: &str, sequence: u64) -> Option<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&Line {
        run_id,
        sequence,
        record,
    })
    .ok()?;
    // Defensive guard: never truncate serialized JSON or persist a partial/oversized line.
    if bytes.len() >= super::MAX_RECORD_BYTES {
        return None;
    }
    bytes.push(b'\n');
    Some(bytes)
}
