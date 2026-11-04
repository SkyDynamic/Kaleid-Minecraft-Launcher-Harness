//! Explicit, domain-neutral diagnostics carried by the typed event bus.
//!
//! Producers must select safe fields explicitly. This is not an observer of arbitrary
//! events, service calls, configs, or errors; redaction at the output boundary remains
//! the writer's responsibility.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Diagnostic severity, independent of any logging backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// A deliberately constructed diagnostic, with explicit component and correlation IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    pub component: String,
    pub event: String,
    pub level: Severity,
    /// Epoch milliseconds, refreshed by [`crate::Context::diagnostic`] at publication.
    pub timestamp_at_emission_ms: u64,
    pub fields: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

impl DiagnosticRecord {
    pub fn new(component: impl Into<String>, event: impl Into<String>, level: Severity) -> Self {
        Self {
            component: component.into(),
            event: event.into(),
            level,
            timestamp_at_emission_ms: epoch_millis(),
            fields: BTreeMap::new(),
            operation_id: None,
            session_id: None,
            job_id: None,
            batch_id: None,
        }
    }

    /// Add a producer-selected safe field; never pass raw configs or errors here.
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }

    pub fn with_operation_id(mut self, id: impl Into<String>) -> Self {
        self.operation_id = Some(id.into());
        self
    }

    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    pub fn with_job_id(mut self, id: impl Into<String>) -> Self {
        self.job_id = Some(id.into());
        self
    }

    pub fn with_batch_id(mut self, id: impl Into<String>) -> Self {
        self.batch_id = Some(id.into());
        self
    }

    pub(crate) fn mark_emitted(&mut self) {
        self.timestamp_at_emission_ms = epoch_millis();
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
