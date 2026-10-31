//! 版本管理与存档管理的动作结果。
//!
//! 这里的结构只描述"用户点了之后发生了什么"，不含业务规则：
//! 拒绝条件（正在运行 / 被继承 / 文件被占用）以 [`crate::error::ApiError`] 回到界面。

use serde::{Deserialize, Serialize};

use crate::model::LoaderKind;

/// 删除一个版本目录的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionDeleteOutcome {
    pub version_id: String,
    /// 被删掉的版本目录。
    pub removed_dir: String,
    /// 被删掉的版本设置文件（没有则为 `None`）。
    pub removed_settings: Option<String>,
    /// 释放的字节数。
    pub freed_bytes: u64,
}

/// 加载器更新检查结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderUpdateStatus {
    pub loader: LoaderKind,
    /// 当前安装的加载器版本。
    pub current: Option<String>,
    /// 该游戏版本下最新的加载器版本。
    pub latest: Option<String>,
    /// 官方推荐的稳定版本。
    pub recommended: Option<String>,
    /// 是否存在更新的版本。
    pub newer_available: bool,
    /// 可选目标版本（新的在前，最多若干条），供界面下拉。
    pub candidates: Vec<String>,
}

/// 存档里的一个数据包。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataPack {
    /// 文件名（`.zip`）或目录名。
    pub file_name: String,
    /// `zip` / `dir`
    pub kind: String,
    pub size_bytes: u64,
    /// `level.dat` 里记录的启用状态；读不到就是 `None`（只展示，不写回）。
    pub enabled: Option<bool>,
    pub pack_format: Option<u32>,
    pub description: Option<String>,
}

/// 删除存档的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDeleteOutcome {
    pub folder: String,
    pub freed_bytes: u64,
}
