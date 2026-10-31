//! 下载、远程版本、加载器与安装请求的公共 DTO。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::LoaderKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteVersion {
    pub id: String,
    /// Mojang 原始类型：release / snapshot / old_beta / old_alpha。
    #[serde(rename = "type")]
    pub kind: String,
    pub release_time: String,
    /// Mojang 版本 JSON 的下载地址（规划安装时使用）。
    #[serde(default)]
    pub url: String,
    /// release / snapshot / ancient / april_fools
    pub category: String,
    pub april_fools_year: Option<u32>,
    pub april_fools_intro: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteVersions {
    pub latest_release: Option<String>,
    pub latest_snapshot: Option<String>,
    pub versions: Vec<RemoteVersion>,
    pub fetched_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoaderVersion {
    pub version: String,
    pub stable: bool,
    pub recommended: bool,
    #[serde(default)]
    pub time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoaderVersions {
    pub game_version: String,
    pub loader: String,
    pub versions: Vec<LoaderVersion>,
    pub recommended: Option<String>,
    pub fetched_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoaderVersionTimes {
    pub times: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FabricApiInfo {
    pub version: String,
    pub file_name: String,
    pub download_url: String,
    pub size: u64,
    pub sha1: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VersionNameCheck {
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallLoader {
    Vanilla,
    Fabric { version: String },
    Forge { version: String },
    Neoforge { version: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub root_id: String,
    pub game_version: String,
    pub version_name: String,
    pub loader: InstallLoader,
    #[serde(default)]
    pub install_fabric_api: bool,
}

impl InstallRequest {
    pub fn loader_kind(&self) -> LoaderKind {
        match self.loader {
            InstallLoader::Vanilla => LoaderKind::Vanilla,
            InstallLoader::Fabric { .. } => LoaderKind::Fabric,
            InstallLoader::Forge { .. } => LoaderKind::Forge,
            InstallLoader::Neoforge { .. } => LoaderKind::NeoForge,
        }
    }

    pub fn loader_version(&self) -> Option<&str> {
        match &self.loader {
            InstallLoader::Vanilla => None,
            InstallLoader::Fabric { version } => Some(version),
            InstallLoader::Forge { version } => Some(version),
            InstallLoader::Neoforge { version } => Some(version),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstallQueued {
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Resolving,
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    Running,
    Completed,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPhase {
    pub id: String,
    pub label: String,
    pub status: PhaseStatus,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub files_done: usize,
    pub files_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadJob {
    pub id: String,
    pub title: String,
    pub version_name: String,
    pub root_id: String,
    pub game_version: String,
    pub loader_kind: LoaderKind,
    pub loader_version: Option<String>,
    pub source: String,
    pub status: DownloadStatus,
    pub current: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub files_done: usize,
    pub files_total: usize,
    pub percent: f64,
    pub error: Option<String>,
    pub phases: Vec<DownloadPhase>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSnapshot {
    pub jobs: Vec<DownloadJob>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub percent: f64,
    pub speed_bps: u64,
    pub concurrency: u32,
    pub active_workers: u32,
    pub active_jobs: usize,
    pub queued_jobs: usize,
}
