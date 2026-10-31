//! 事件载荷。
//!
//! 总线按**类型**订阅，所以这里每一个 struct 都是一个事件。界面与其它插件据此刷新，
//! 而不是互相轮询。

use crate::model::{Account, EffectiveConfig, Root};

/// 文件夹列表或其中某项发生变化（增删、改名、切隔离、切换选中）。
#[derive(Debug, Clone)]
pub struct RootsChanged {
    pub roots: Vec<Root>,
    pub selected: Option<String>,
}

/// 某个文件夹下的版本列表需要重扫。
#[derive(Debug, Clone)]
pub struct VersionsChanged {
    pub root_id: String,
}

/// 当前账号或账号列表变化。**全局**：所有文件夹、所有版本共用同一账号。
#[derive(Debug, Clone)]
pub struct AccountChanged {
    pub active: Option<Account>,
}

/// 全局设置变化。
#[derive(Debug, Clone)]
pub struct GlobalSettingsChanged {
    pub isolation_default: bool,
    pub download_concurrency: u32,
}

/// 某个版本的生效配置变化。
#[derive(Debug, Clone)]
pub struct VersionSettingsChanged {
    pub root_id: String,
    pub version_id: String,
    pub effective: EffectiveConfig,
}

/// 下载进度（有声明的下载源插件在跑任务时发出）。
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub task_id: String,
    pub label: String,
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
    pub files_done: usize,
    pub files_total: usize,
}

/// 下载任务结束。
#[derive(Debug, Clone)]
pub struct DownloadFinished {
    pub task_id: String,
    pub ok: bool,
    pub message: Option<String>,
}
