use serde::{Deserialize, Serialize};

/// Stable machine-readable failures shared by manifest, installer, and runtime layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginErrorCode {
    InvalidManifest,
    UnsupportedApi,
    AppVersionMismatch,
    Conflict,
    InventoryInvalid,
    EntryInvalid,
    UnsupportedPlatform,
    PackageTooLarge,
    SignatureUnsupported,
    UnsignedNotAllowed,
    AlreadyInstalled,
    Busy,
    Unauthorized,
    RuntimeUnavailable,
    Protocol,
    Timeout,
    BackendExited,
    Io,
}

/// A conflict reported by inventory/installation, including the declaration's owner.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConflictDetail {
    pub id: String,
    pub reason: String,
    pub declared_by: String,
}

/// Plugin-specific structured error; ordinary `ApiError` remains a serialized string.
/// Messages should be bounded summaries, not source documents or backend result dumps.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct PluginError {
    pub code: PluginErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    #[serde(default)]
    pub conflicts: Vec<ConflictDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_range: Option<String>,
    #[serde(default = "known_outcome")]
    pub outcome_known: bool,
}

fn known_outcome() -> bool {
    true
}

impl PluginError {
    pub fn new(code: PluginErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            plugin_id: None,
            conflicts: Vec::new(),
            app_version: None,
            required_range: None,
            outcome_known: true,
        }
    }

    pub fn with_plugin_id(mut self, id: impl Into<String>) -> Self {
        self.plugin_id = Some(id.into());
        self
    }
}
