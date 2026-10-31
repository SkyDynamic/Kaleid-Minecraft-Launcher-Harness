//! 服务契约。
//!
//! 全部方法同步、对象安全：内核不绑定异步运行时，需要并发的服务自己在实现里持运行时。
//! 插件通过 `ctx.provide(...)` 注册实现，界面通过 Tauri 命令间接调用。

use std::path::Path;

use serde_json::Value;

use crate::error::ApiResult;
use crate::model::{
    Account, EffectiveConfig, GlobalSettings, IsolationMode, LaunchPlan, Root, VersionSettings,
    VersionSummary,
};

/// 文件夹收录与选择。
pub trait RootRegistryService: Send + Sync {
    fn list(&self) -> ApiResult<Vec<Root>>;
    fn get(&self, id: &str) -> ApiResult<Root>;
    /// 收录一个路径。`Pick` 的分类规则见 [`crate::model::Pick::classify`]；
    /// `OrdinaryDir` 需要 `force=true` 才接受。
    fn add(&self, path: &Path, alias: Option<&str>, force: bool) -> ApiResult<Root>;
    /// 只取消注册，**不动磁盘**。
    fn remove(&self, id: &str) -> ApiResult<()>;
    fn rename(&self, id: &str, alias: &str) -> ApiResult<Root>;
    fn set_isolation(&self, id: &str, mode: IsolationMode) -> ApiResult<Root>;
    fn select(&self, id: &str) -> ApiResult<Root>;
    fn selected(&self) -> ApiResult<Option<Root>>;
    /// 设为默认目录：列表里置顶，下次打开启动器优先选中它。
    fn set_default(&self, id: &str) -> ApiResult<Root>;
    /// 当前默认目录；没设过返回 `None`。
    fn default_root(&self) -> ApiResult<Option<Root>>;
    /// 取消默认（不删目录）。
    fn clear_default(&self) -> ApiResult<()>;
}

/// 版本扫描与分类。
pub trait VersionStoreService: Send + Sync {
    fn scan(&self, root_path: &Path) -> ApiResult<Vec<VersionSummary>>;
    /// 只解析单个版本 JSON。
    fn inspect(&self, json_path: &Path) -> ApiResult<VersionSummary>;
}

/// 分层设置解析：版本 > 文件夹 > 全局 > 内置默认。
pub trait SettingsServiceApi: Send + Sync {
    fn global(&self) -> ApiResult<GlobalSettings>;
    fn patch_global(&self, patch: &Value) -> ApiResult<GlobalSettings>;
    fn version_settings(&self, root_id: &str, version_id: &str) -> ApiResult<VersionSettings>;
    fn patch_version(
        &self,
        root_id: &str,
        version_id: &str,
        patch: &Value,
    ) -> ApiResult<VersionSettings>;
    /// 合并三层得到生效配置；`folder_isolation` 由 [`RootRegistryService`] 提供。
    fn resolve(
        &self,
        root_id: &str,
        version_id: &str,
        folder_isolation: IsolationMode,
    ) -> ApiResult<EffectiveConfig>;
}

/// 账号：全局唯一一份，正版可多个，离线只有一个生效名。
pub trait AccountServiceApi: Send + Sync {
    fn list(&self) -> ApiResult<Vec<Account>>;
    fn active(&self) -> ApiResult<Option<Account>>;
    fn set_active(&self, id: &str) -> ApiResult<Account>;
    /// **输入即生效**：校验通过就写入并设为当前账号。
    /// 无独立的"保存/取消"，因为界面上没有这两个动作。
    fn apply_offline_name(&self, name: &str) -> ApiResult<Account>;
    fn remove(&self, id: &str) -> ApiResult<()>;
}

/// 启动参数解析。
pub trait LaunchPlannerApi: Send + Sync {
    fn plan(&self, root_path: &Path, version_id: &str, account: &Account) -> ApiResult<LaunchPlan>;
}

/// 下载源（由"版本管理"之类的插件贡献，第三方可以再加一个镜像）。
pub trait InstallSourceApi: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> &str;
    fn priority(&self) -> i32;
    fn manifest_url(&self) -> &str;
    fn base_url(&self) -> &str;
}
