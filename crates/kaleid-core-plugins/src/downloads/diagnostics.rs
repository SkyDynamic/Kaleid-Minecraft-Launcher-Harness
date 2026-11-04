//! Producer-side safe records only: no logger implementation, payloads, or file I/O.
use kaleid_api::{ApiError, ApiResult};
use kaleid_kernel::{Context, DiagnosticRecord, Severity};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(crate) type Recorder = Option<Context>;
pub(crate) fn publish(ctx: &Recorder, record: DiagnosticRecord) {
    if let Some(ctx) = ctx {
        ctx.diagnostic(record);
    }
}
pub(crate) fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}
pub(crate) fn category(error: &ApiError) -> &'static str {
    match error {
        ApiError::Path(_) | ApiError::Io(_) => "io",
        ApiError::Format(_) => "format",
        ApiError::NotFound(_) => "not_found",
        ApiError::Invalid(_) => "invalid",
        ApiError::Conflict(_) => "conflict",
        ApiError::Unavailable(_) => "unavailable",
        ApiError::Network(_) => "network",
    }
}
/// Recognize a cancellation-shaped legacy result; terminal cause requires independent evidence.
pub(crate) fn observed_cancel<T>(result: &ApiResult<T>) -> bool {
    matches!(result, Err(ApiError::Invalid(message)) if message == "任务已取消")
}

#[derive(Clone)]
pub(crate) struct Operation {
    pub(crate) ctx: Recorder,
    pub(crate) record: DiagnosticRecord,
    started: Instant,
    finished: Arc<AtomicBool>,
    terminal_failure: Arc<AtomicBool>,
}
impl Operation {
    pub(crate) fn new(ctx: Recorder, component: &'static str, operation: &'static str) -> Self {
        Self {
            ctx,
            record: DiagnosticRecord::new(component, "operation.accepted", Severity::Info)
                .with_operation_id(uuid::Uuid::new_v4().to_string())
                .with_field("operation", operation),
            started: Instant::now(),
            finished: Arc::new(AtomicBool::new(false)),
            terminal_failure: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(crate) fn accepted(&self) {
        publish(
            &self.ctx,
            self.record.clone().with_field("outcome", "accepted"),
        );
    }
    pub(crate) fn note_terminal_failure(&self) { self.terminal_failure.store(true, Ordering::SeqCst); }

    pub(crate) fn finish<T>(&self, result: &ApiResult<T>, cancelled: bool) {
        self.finish_with_failure(result, cancelled, false);
    }

    pub(crate) fn finish_with_failure<T>(&self, result: &ApiResult<T>, cancelled: bool, terminal_failure: bool) {
        let cancelled_outcome = observed_cancel(result) && !terminal_failure && !self.terminal_failure.load(Ordering::SeqCst);
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut record = self.record.clone();
        record.event = "operation.finished".into();
        record.level = if result.is_err() && !cancelled_outcome {
            Severity::Error
        } else {
            Severity::Info
        };
        record = record
            .with_field(
                "outcome",
                if result.is_ok() {
                    "succeeded"
                } else if cancelled_outcome {
                    "cancelled"
                } else {
                    "failed"
                },
            )
            .with_field("cancelled", cancelled)
            .with_field("duration_ms", millis(self.started));
        if !record.fields.contains_key("category") {
            if let Err(error) = result {
                record = record.with_field("category", category(error));
            }
        }
        publish(&self.ctx, record);
    }
}

#[derive(Default)]
struct Warnings {
    seen: HashSet<(String, &'static str, &'static str)>,
    retry: u64,
    throttle: u64,
    fallback: u64,
    range: u64,
    integrity: u64,
    internal: u64,
    failed: u64,
    suppressed: u64,
    attempts: u64,
}
pub(super) struct BatchDiagnostics {
    ctx: Recorder,
    job_id: String,
    id: String,
    started: Instant,
    warnings: Mutex<Warnings>,
    terminal_failure: AtomicBool,
}
impl BatchDiagnostics {
    pub(super) fn new(ctx: Recorder, job_id: &str) -> Arc<Self> {
        let this = Arc::new(Self {
            ctx,
            job_id: job_id.into(),
            id: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            warnings: Mutex::new(Warnings::default()),
            terminal_failure: AtomicBool::new(false),
        });
        publish(
            &this.ctx,
            this.record("download.batch.started", Severity::Info)
                .with_field("outcome", "started"),
        );
        this
    }
    pub(super) fn note_terminal_failure(&self) { self.terminal_failure.store(true, Ordering::SeqCst); }

    fn record(&self, event: &'static str, level: Severity) -> DiagnosticRecord {
        DiagnosticRecord::new("downloads", event, level)
            .with_job_id(&self.job_id)
            .with_batch_id(&self.id)
    }
    pub(super) fn warning(
        &self,
        event: &'static str,
        category: &'static str,
        url: Option<&str>,
        fields: &[(&str, serde_json::Value)],
    ) {
        let host = url.and_then(crate::diagnostic_host);
        let emit = if let Ok(mut warnings) = self.warnings.lock() {
            match event {
                "download.retry" => warnings.retry += 1,
                "download.throttle" => warnings.throttle += 1,
                "download.source_fallback" => warnings.fallback += 1,
                "download.range_fallback" => warnings.range += 1,
                "download.integrity_failure" => warnings.integrity += 1,
                "download.failed" => warnings.failed += 1,
                "download.attempt_failed" => warnings.attempts += 1,
                _ => warnings.internal += 1,
            }
            let key = (host.clone().unwrap_or_default(), event, category);
            if warnings.seen.contains(&key) || warnings.seen.len() >= 32 {
                warnings.suppressed += 1;
                false
            } else {
                warnings.seen.insert(key);
                true
            }
        } else {
            false
        };
        // Never invoke observers while holding the aggregation lock.
        if !emit {
            return;
        }
        let mut record = self
            .record(event, Severity::Warn)
            .with_field("category", category);
        if let Some(host) = host {
            record = record.with_field("host", host);
        }
        for (key, value) in fields {
            record = record.with_field(*key, value.clone());
        }
        publish(&self.ctx, record);
    }
    pub(super) fn finish(
        &self,
        result: &Result<(), String>,
        cancelled: bool,
        files_done: usize,
        files_total: usize,
        done_bytes: Option<u64>,
        total_bytes: Option<u64>,
    ) {
        let observed_cancel = matches!(result, Err(message) if message == "任务已取消") && !self.terminal_failure.load(Ordering::SeqCst);
        let mut record = self
            .record(
                "download.batch.finished",
                if result.is_err() && !observed_cancel {
                    Severity::Error
                } else {
                    Severity::Info
                },
            )
            .with_field(
                "outcome",
                if result.is_ok() {
                    "succeeded"
                } else if observed_cancel {
                    "cancelled"
                } else {
                    "failed"
                },
            )
            .with_field("cancelled", cancelled)
            .with_field("duration_ms", millis(self.started))
            .with_field("files_done", files_done as u64)
            .with_field("files_total", files_total as u64);
        if let Some(bytes) = done_bytes {
            record = record.with_field("done_bytes", bytes);
        }
        if let Some(bytes) = total_bytes {
            record = record.with_field("total_bytes", bytes);
        }
        if let Ok(w) = self.warnings.lock() {
            record = record
                .with_field("retry_count", w.retry)
                .with_field("throttle_count", w.throttle)
                .with_field("source_fallback_count", w.fallback)
                .with_field("range_fallback_count", w.range)
                .with_field("integrity_failure_count", w.integrity)
                .with_field("internal_error_count", w.internal)
                .with_field("warnings_suppressed", w.suppressed)
                .with_field("failed", w.failed).with_field("count", w.attempts);
        }
        publish(&self.ctx, record);
    }
}

/// Same thread-builder behavior; only tests may inject one local creation failure.
pub(crate) fn spawn_thread<T: Send + 'static>(
    name: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<T>> {
    #[cfg(test)]
    if FAIL_NEXT_SPAWN.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("test thread creation failure"));
    }
    std::thread::Builder::new().name(name.into()).spawn(work)
}
#[cfg(test)]
thread_local! { static FAIL_NEXT_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(crate) fn fail_next_spawn() {
    FAIL_NEXT_SPAWN.with(|flag| flag.set(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_batch_error_with_pending_cancel_is_still_error() {
        let kernel = kaleid_kernel::Kernel::new();
        let records = Arc::new(Mutex::new(Vec::<DiagnosticRecord>::new()));
        let captured = records.clone();
        kernel.context().on::<DiagnosticRecord>(move |r| {
            if let Ok(mut records) = captured.lock() {
                records.push(r.clone());
            }
        });
        let batch = BatchDiagnostics::new(Some(kernel.context().clone()), "batch-job");
        batch.finish(
            &Err("non-cancel SECRET_ERROR".into()),
            true,
            0,
            1,
            None,
            None,
        );
        let records = records.lock().unwrap();
        let ended = records
            .iter()
            .find(|r| r.event == "download.batch.finished")
            .unwrap();
        assert_eq!(ended.fields["outcome"], "failed");
        assert_eq!(ended.fields["cancelled"], true);
        assert_eq!(ended.level, Severity::Error);
        assert!(!serde_json::to_string(&*records)
            .unwrap()
            .contains("SECRET_ERROR"));
    }

    #[test]
    fn diagnostics_batch_warning_hosts_are_bounded_and_repetitions_summarized() {
        let kernel = kaleid_kernel::Kernel::new();
        let records = Arc::new(Mutex::new(Vec::<DiagnosticRecord>::new()));
        let captured = records.clone();
        kernel.context().on::<DiagnosticRecord>(move |r| {
            if let Ok(mut records) = captured.lock() {
                records.push(r.clone());
            }
        });
        let batch = BatchDiagnostics::new(Some(kernel.context().clone()), "batch-job");
        for host in 0..1000 {
            batch.warning(
                "download.retry",
                "transfer",
                Some(&format!(
                    "https://node-{host}.example/file?token=SECRET_QUERY"
                )),
                &[],
            );
        }
        batch.finish(&Ok(()), false, 1, 1, Some(1), Some(1));
        let records = records.lock().unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|r| r.event == "download.retry")
                .count(),
            32
        );
        let ended = records
            .iter()
            .find(|r| r.event == "download.batch.finished")
            .unwrap();
        assert_eq!(ended.fields["retry_count"], 1000);
        assert_eq!(ended.fields["warnings_suppressed"], 968);
        assert!(!serde_json::to_string(&*records)
            .unwrap()
            .contains("SECRET_QUERY"));
    }
}
