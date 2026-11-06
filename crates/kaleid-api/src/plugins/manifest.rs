use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Preserves the API-specific public structs and their original, unwrapped JSON shape.
/// Use `decode_manifest` for strict API-discriminated deserialization.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PluginManifest {
    V1(ManifestV1),
    V2(ManifestV2),
}

impl PluginManifest {
    pub fn id(&self) -> &str {
        match self {
            Self::V1(m) => &m.id,
            Self::V2(m) => &m.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::V1(m) => &m.name,
            Self::V2(m) => &m.name,
        }
    }

    pub fn version(&self) -> &str {
        match self {
            Self::V1(m) => &m.version,
            Self::V2(m) => &m.version,
        }
    }

    pub fn api(&self) -> &str {
        match self {
            Self::V1(m) => &m.api,
            Self::V2(m) => &m.api,
        }
    }

    pub fn steps(&self) -> &[ManifestStep] {
        match self {
            Self::V1(m) => &m.steps,
            Self::V2(m) => &m.steps,
        }
    }

    /// API 1 has no conflict declarations.
    pub fn conflicts(&self) -> &[ConflictDeclaration] {
        match self {
            Self::V1(_) => &[],
            Self::V2(m) => &m.conflicts,
        }
    }
}

/// Legacy declarative shape, including a required steps field and non-SemVer version strings.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestV1 {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    pub steps: Vec<ManifestStep>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestV2 {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    pub app_version: String,
    pub conflicts: Vec<ConflictDeclaration>,
    #[serde(default)]
    pub platforms: Vec<PluginPlatform>,
    #[serde(default)]
    pub entrypoints: PluginEntrypoints,
    #[serde(default)]
    pub contributes: PluginContributions,
    #[serde(default)]
    pub replaces: Vec<String>,
    #[serde(default)]
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub steps: Vec<ManifestStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictDeclaration {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum PluginPlatform {
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
    #[serde(rename = "windows-x86")]
    WindowsX86,
    #[serde(rename = "windows-aarch64")]
    WindowsAarch64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    #[serde(rename = "linux-aarch64")]
    LinuxAarch64,
    #[serde(rename = "macos-x86_64")]
    MacosX86_64,
    #[serde(rename = "macos-aarch64")]
    MacosAarch64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntrypoints {
    #[serde(default)]
    pub frontend: Option<String>,
    #[serde(default)]
    pub backend: Option<BackendEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendEntry {
    pub runtime: BackendRuntime,
    pub entry: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendRuntime {
    Executable,
    Node,
    Python,
    Java,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginContributions {
    #[serde(default)]
    pub ui: Vec<UiContribution>,
    #[serde(default)]
    pub commands: Vec<CommandContribution>,
    #[serde(default)]
    pub services: Vec<ServiceContribution>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UiContribution {
    pub target: String,
    pub mode: UiMode,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default = "empty_object")]
    pub config: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UiMode {
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandContribution {
    pub name: String,
    pub input_schema: Value,
    pub output_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceContribution {
    pub name: String,
    pub contract_version: u32,
    pub methods: Vec<CommandContribution>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginPermissions {
    #[serde(default)]
    pub host_commands: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
}

/// Shared closed declarative operations for API 1 and API 2. Legacy core adoption is separate.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManifestStep {
    UpdateConfig {
        target: String,
        patch: Value,
    },
    DisablePlugin {
        target: String,
    },
    EnablePlugin {
        target: String,
    },
    ProvideSource {
        id: String,
        label: String,
        #[serde(default)]
        priority: i32,
        #[serde(rename = "manifestUrl")]
        manifest_url: String,
        #[serde(rename = "baseUrl")]
        base_url: String,
    },
    BindSlot {
        slot: String,
        title: String,
        view: String,
    },
    SetDefaultSetting {
        key: String,
        value: Value,
    },
}
