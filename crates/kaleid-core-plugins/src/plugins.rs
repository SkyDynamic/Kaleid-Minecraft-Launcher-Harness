//! 内置插件定义、工厂注册与组合 preset。
//!
//! 每个插件只做一件小事，并且都以**服务**的形式暴露能力。这样：
//! - 停用「版本管理」不会删掉文件夹/版本列表（它们由 `core-lists` / `version-store` 提供）；
//! - 第三方想换下载源，只需再 `provide` 一张来源表，不用改我们的代码。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use kaleid_api::{InstallSourceApi, JavaServiceApi, RootRegistryService, SettingsServiceApi};
use kaleid_kernel::{
    apply_patches, parse_patch, Context, Kernel, Plugin, PluginResult, PluginSpec, PluginState,
};

use crate::accounts::Accounts;
use crate::admin::VersionAdmin;
use crate::game::GameProcessManager;
use crate::saves::SaveManager;
use crate::content::ContentService;
use crate::downloads::DownloadManager;
use crate::install::InstallService;
use crate::java::{JavaService, ScanEnv};
use crate::launch::LaunchPlanner;
use crate::modnames::ModNames;
use crate::mods::ModService;
use crate::msa::{FileTokenStore, MsaAuth};
use crate::roots::RootRegistry;
use crate::settings::Settings;
use crate::versions::VersionStore;

// ---------------------------------------------------------------------------
// 下载源
// ---------------------------------------------------------------------------

/// 一条下载源。
pub struct BuiltinSource {
    id: &'static str,
    label: &'static str,
    priority: i32,
    manifest: &'static str,
    base: &'static str,
}

impl InstallSourceApi for BuiltinSource {
    fn id(&self) -> &str {
        self.id
    }
    fn label(&self) -> &str {
        self.label
    }
    fn priority(&self) -> i32 {
        self.priority
    }
    fn manifest_url(&self) -> &str {
        self.manifest
    }
    fn base_url(&self) -> &str {
        self.base
    }
}

/// 插件贡献的一组下载源。多个插件可以各贡献一组，消费方合并全部。
#[derive(Clone)]
pub struct InstallSourceTable {
    pub owner: String,
    pub sources: Vec<Arc<dyn InstallSourceApi>>,
}

impl InstallSourceTable {
    pub fn all(&self) -> Vec<Arc<dyn InstallSourceApi>> {
        self.sources.clone()
    }
}

pub fn builtin_sources() -> InstallSourceTable {
    InstallSourceTable {
        owner: "version-manager".into(),
        sources: vec![
            Arc::new(BuiltinSource {
                id: "mojang",
                label: "官方源（Mojang）",
                priority: 0,
                manifest: "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json",
                base: "https://piston-meta.mojang.com",
            }) as Arc<dyn InstallSourceApi>,
            Arc::new(BuiltinSource {
                id: "bmclapi",
                label: "BMCLAPI 镜像",
                priority: 10,
                // 主站还带八个校园镜像（见 [`crate::mirrors`]），但它们只有静态文件树、
                // 没有清单，所以只在这一张"下载源表"里出现一次。
                manifest: crate::mirrors::BMCLAPI_MANIFEST_URL,
                base: crate::mirrors::BMCLAPI_BASE,
            }),
        ],
    }
}

// ---------------------------------------------------------------------------
// 插件
// ---------------------------------------------------------------------------

pub struct CoreLists {
    data_root: PathBuf,
}

impl Plugin for CoreLists {
    fn id(&self) -> &str {
        "core-lists"
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let registry = RootRegistry::open(&self.data_root, Some(ctx.clone()))?;
        ctx.provide("roots", registry, 0);
        Ok(())
    }
}

pub struct VersionStorePlugin;

impl Plugin for VersionStorePlugin {
    fn id(&self) -> &str {
        "version-store"
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        ctx.provide("versions", VersionStore::new(), 0);
        Ok(())
    }
}

pub struct SettingsPlugin {
    data_root: PathBuf,
}

impl Plugin for SettingsPlugin {
    fn id(&self) -> &str {
        "settings"
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let settings = Settings::open(&self.data_root).with_context(ctx.clone());
        ctx.provide("settings", settings, 0);
        Ok(())
    }

    fn schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "defaultMemoryMb": { "type": "integer", "minimum": 512, "maximum": 32768, "default": 4096 }
            }
        }))
    }
}

pub struct AccountsPlugin {
    data_root: PathBuf,
}

impl Plugin for AccountsPlugin {
    fn id(&self) -> &str {
        "accounts"
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let accounts = Accounts::open(&self.data_root)?.with_context(ctx.clone());
        ctx.provide("accounts", accounts, 0);
        Ok(())
    }
}

/// 启动与参数管理。
pub struct LaunchPlugin {
    _private: (),
}

impl Plugin for LaunchPlugin {
    fn id(&self) -> &str {
        "launch"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["settings", "core-lists"]
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let settings = ctx.service::<Settings>().ok_or("settings 服务不可用")?;
        let roots = ctx.service::<RootRegistry>().ok_or("roots 服务不可用")?;
        // `java-runtime` 在 preset 里排在 launch 前面，所以这里能拿到；
        // 但它**不是 inject 依赖**——停用 Java 发现时启动仍然要能出计划（只是没有自动匹配）。
        let java = ctx
            .service::<JavaService>()
            .map(|service| service as Arc<dyn JavaServiceApi>);
        let planner = LaunchPlanner::new(settings, roots).with_java(java);
        ctx.provide("launch", planner, 0);
        Ok(())
    }

    fn schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "checkBeforeLaunch": { "type": "boolean", "default": true }
            }
        }))
    }
}

/// 版本管理：本组合里它**只贡献下载源与安装能力**，
/// 文件夹/版本列表本身属于 `core-lists` / `version-store`，所以停用它不会让侧栏空掉。
pub struct VersionManagerPlugin {
    data_root: PathBuf,
}

/// 新闻模块作为内置插件：默认开启，关闭后 UI 的 `dock.news` 槽位直接消失。
pub struct NewsPlugin;

impl Plugin for NewsPlugin {
    fn id(&self) -> &str {
        "news"
    }

    fn apply(&self, _ctx: &Context, _cfg: Value) -> PluginResult {
        Ok(())
    }

    fn schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "showInDock": { "type": "boolean", "default": true }
            }
        }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionManagerConfig {
    #[serde(default = "default_true")]
    pub enable_mojang: bool,
    #[serde(default = "default_true")]
    pub enable_bmclapi: bool,
    /// 自动测速挑选最快的源，失败自动回退。
    #[serde(default = "default_true")]
    pub auto_speed_test: bool,
    #[serde(default = "default_true")]
    pub verify_after_install: bool,
}

fn default_true() -> bool {
    true
}

impl Default for VersionManagerConfig {
    fn default() -> Self {
        VersionManagerConfig {
            enable_mojang: true,
            enable_bmclapi: true,
            auto_speed_test: true,
            verify_after_install: true,
        }
    }
}

impl Plugin for VersionManagerPlugin {
    fn id(&self) -> &str {
        "version-manager"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["core-lists", "version-store", "settings"]
    }

    fn default_config(&self) -> Value {
        serde_json::to_value(VersionManagerConfig::default()).unwrap_or_else(|_| json!({}))
    }

    fn schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "enableMojang": { "type": "boolean", "default": true },
                "enableBmclapi": { "type": "boolean", "default": true },
                "autoSpeedTest": { "type": "boolean", "default": true },
                "verifyAfterInstall": { "type": "boolean", "default": true }
            }
        }))
    }

    fn apply(&self, ctx: &Context, cfg: Value) -> PluginResult {
        let cfg: VersionManagerConfig =
            serde_json::from_value(cfg).map_err(|e| format!("version-manager 配置不合法：{e}"))?;
        let mut table = builtin_sources();
        table.sources.retain(|s| match s.id() {
            "mojang" => cfg.enable_mojang,
            "bmclapi" => cfg.enable_bmclapi,
            _ => true,
        });
        if table.sources.is_empty() {
            return Err("至少需要启用一个下载源".into());
        }
        ctx.provide("install-sources", table, 0);
        let settings = ctx.service::<Settings>().ok_or("settings 服务不可用")?;
        let versions = ctx.service::<VersionStore>().ok_or("versions 服务不可用")?;
        let roots = ctx.service::<RootRegistry>().ok_or("roots 服务不可用")?;
        // 重定向终点缓存放进数据目录：跨进程复用，同一个版本的资源不必反复探测。
        let downloads = Arc::new(
            DownloadManager::new(settings.clone()).with_data_root(&self.data_root).with_context(ctx.clone()),
        );
        // 上次被强杀留下的安装现场（几百 MB 的 jar）顺手清掉，别越攒越多。
        crate::install::sweep_staging(&self.data_root);
        // 上次被强杀留下的“安装中”版本：完整就删标记，半成品就清掉。
        crate::install::sweep_incomplete_installs(roots.as_ref());
        let java = ctx
            .service::<JavaService>()
            .map(|service| service as Arc<dyn JavaServiceApi>);
        let install = Arc::new(
            InstallService::new(settings, versions.clone(), roots, downloads.clone(), &self.data_root)
                .with_java(java).with_context(ctx.clone()),
        );
        // Context 按 **值类型** 建索引：要提供 `DownloadManager` 本体，而不是
        // 外面那层 Arc，否则宿主 `service::<DownloadManager>()` 会找不到。
        ctx.provide("downloads", (*downloads).clone(), 0);
        ctx.provide("install", (*install).clone(), 0);
        // 版本管理还带一套「动已经存在的版本目录」的动作（删除 / 换来加载器版本），
        // 和安装共用同一份版本目录布局，所以一起挂在这个插件下面。
        ctx.provide(
            "version-admin",
            VersionAdmin::new(versions, &self.data_root),
            0,
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 模组下载
// ---------------------------------------------------------------------------

/// 模组下载插件：只依赖扫描/设置/文件夹三件基础设施。
///
/// `DownloadManager` 由「版本管理」提供，**故意不做 inject**：停用版本管理时
/// 搜索与详情仍然可用，只有真正下载时报一句人话。
pub struct ModDownloadPlugin {
    pub data_root: PathBuf,
}

impl Plugin for ModDownloadPlugin {
    fn id(&self) -> &str {
        "mod-download"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["core-lists", "version-store", "settings"]
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let settings = ctx.service::<Settings>().ok_or("settings 服务不可用")?;
        let versions = ctx.service::<VersionStore>().ok_or("versions 服务不可用")?;
        let roots = ctx.service::<RootRegistry>().ok_or("roots 服务不可用")?;
        let downloads = ctx.service::<DownloadManager>();
        let service = ModService::new(settings, roots, versions, downloads, &self.data_root).with_context(ctx.clone());
        ctx.provide("mods", service, 0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 本地内容与 Java 运行时
// ---------------------------------------------------------------------------

/// 本地游戏内容：模组 / 资源包 / 存档 / 日志。
///
/// 只读磁盘、只动 `mods/` 与 `resourcepacks/` 里用户点过的东西；位置跟随版本隔离。
pub struct GameContentPlugin {
    pub data_root: PathBuf,
}

impl Plugin for GameContentPlugin {
    fn id(&self) -> &str {
        "game-content"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["core-lists", "settings"]
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let settings = ctx.service::<Settings>().ok_or("settings 服务不可用")?;
        let roots = ctx.service::<RootRegistry>().ok_or("roots 服务不可用")?;
        let names = Arc::new(ModNames::load(Some(&self.data_root)));
        // 下载器与版本扫描都**不是 inject 依赖**：停用版本管理后列表/图标/存档
        // 这些照样要能用，只是「装更新」会报一句人话、更新检查会拿不到游戏版本。
        // 它们在 preset 里排在 game-content 前面，所以标准组合下这里拿得到。
        let downloads = ctx.service::<DownloadManager>();
        let versions = ctx.service::<VersionStore>();
        let service = ContentService::new(settings, roots, names)
            .with_downloads(downloads)
            .with_versions(versions).with_context(ctx.clone());
        ctx.provide("content", service, 0);
        Ok(())
    }
}

/// Java 运行时发现：扫注册表、常见安装目录、官方启动器自带运行时、PATH。
///
/// 不 inject `settings`：设置只是"顺带把手动指定的路径也列出来"，
/// 停用设置插件的话这个服务少一份来源，但不该整体不可用。
pub struct JavaRuntimePlugin;

impl Plugin for JavaRuntimePlugin {
    fn id(&self) -> &str {
        "java-runtime"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["core-lists"]
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let roots = ctx.service::<RootRegistry>().ok_or("roots 服务不可用")?;
        let mut service = JavaService::new(ScanEnv::current(Vec::new(), None)).with_roots(roots);
        if let Some(settings) = ctx.service::<Settings>() {
            service = service.with_settings(settings);
        }
        ctx.provide("java", service, 0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 微软登录
// ---------------------------------------------------------------------------

/// 微软账号登录（OAuth2 设备码流程）。
///
/// inject `accounts`：登录的产物就是"登记一个正版账号"，没有账号服务的话
/// 这个插件无从落库。
///
/// **必需插件**：正版登录是启动器的核心路径，允许关掉只会得到"账号页看得见、
/// 点了登不上"的半残状态。规格里标了 `required`，`Kernel::disable` 会拒绝停用，
/// 插件页据此把开关置灰（理由文案见 `ui/src/shell/settings/requiredPlugins.ts`）。
pub struct MsaLoginPlugin {
    pub data_root: PathBuf,
}

impl Plugin for MsaLoginPlugin {
    fn id(&self) -> &str {
        "msa-login"
    }

    fn inject(&self) -> &'static [&'static str] {
        &["accounts"]
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let accounts = ctx.service::<Accounts>().ok_or("accounts 服务不可用")?;
        let store = Arc::new(FileTokenStore::new(&self.data_root));
        ctx.provide("msa", MsaAuth::from_build(accounts, store), 0);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 工厂注册与 preset
// ---------------------------------------------------------------------------

/// 注册全部内置插件工厂。
pub fn register_all(kernel: &Kernel, data_root: &Path) {
    let dr = data_root.to_path_buf();
    kernel.register_factory("launcher-logger", {
        let dr = dr.clone();
        move || Arc::new(crate::logger::LauncherLoggerPlugin::new(&dr)) as Arc<dyn Plugin>
    });
    kernel.register_factory("core-lists", {
        let dr = dr.clone();
        move || {
            Arc::new(CoreLists {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("version-store", || {
        Arc::new(VersionStorePlugin) as Arc<dyn Plugin>
    });
    kernel.register_factory("settings", {
        let dr = dr.clone();
        move || {
            Arc::new(SettingsPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("accounts", {
        let dr = dr.clone();
        move || {
            Arc::new(AccountsPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("game-content", {
        let dr = dr.clone();
        move || {
            Arc::new(GameContentPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("java-runtime", || {
        Arc::new(JavaRuntimePlugin) as Arc<dyn Plugin>
    });
    kernel.register_factory("msa-login", {
        let dr = dr.clone();
        move || {
            Arc::new(MsaLoginPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("launch", || {
        Arc::new(LaunchPlugin { _private: () }) as Arc<dyn Plugin>
    });
    kernel.register_factory("news", || Arc::new(NewsPlugin) as Arc<dyn Plugin>);
    kernel.register_factory("version-manager", {
        let dr = dr.clone();
        move || {
            Arc::new(VersionManagerPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("game-launch", {
        let dr = dr.clone();
        move || {
            Arc::new(GameLaunchPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("save-manager", {
        let dr = dr.clone();
        move || {
            Arc::new(SaveManagerPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
    kernel.register_factory("mod-download", {
        let dr = dr.clone();
        move || {
            Arc::new(ModDownloadPlugin {
                data_root: dr.clone(),
            }) as Arc<dyn Plugin>
        }
    });
}

/// 真实启动：spawn Java、收集控制台、判定崩溃、导出排障包。
///
/// 只依赖 `data_root`（日志与导出都落在数据目录），不 inject 别的东西：
/// 计划是 host 用 `launch` 插件算好传进来的，这里只负责"真的跑起来"。
pub struct GameLaunchPlugin {
    pub data_root: PathBuf,
}

impl Plugin for GameLaunchPlugin {
    fn id(&self) -> &str {
        "game-launch"
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        // 补全缺失文件要用下载器。它由「版本管理」提供，**不做 inject**：
        // 那个插件停用后启动仍然可用，只是缺文件时给一句人话而不是自动下载。
        ctx.provide(
            "game",
            GameProcessManager::new(&self.data_root).with_context(ctx.clone()),
            0,
        );
        Ok(())
    }
}

/// 存档管理：列出/删除存档，读写存档内的数据包清单。
///
/// 启用状态只读 `level.dat`（项目没有 NBT 写入器，改写有毁档风险），
/// 启用/禁用交给游戏自己写。
pub struct SaveManagerPlugin {
    pub data_root: PathBuf,
}

impl Plugin for SaveManagerPlugin {
    fn id(&self) -> &str {
        "save-manager"
    }

    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        ctx.provide("saves", SaveManager::new(&self.data_root), 0);
        Ok(())
    }
}
/// 标准组合（默认）。
///
/// 标了 `.required()` 的是**地基**：它们各自只贡献一块能力，但缺了之后启动器
/// 要么少了可以启动的目标（`version-store`），要么生成不出启动参数（`settings` /
/// `java-runtime`），要么界面上留着入口却永远点不动。这一类在插件管理页带
/// 「必需」标、开关置灰，`Kernel::disable` 也会直接拒绝。
///
/// `java-runtime` 与 `game-content` 是后来补进来的：Java 运行时挑不出来就启动
/// 不了，本地内容读不出来则版本页四个标签全空——都是「看得见但用不了」，所以
/// 归到必需。
pub fn standard_preset() -> Vec<PluginSpec> {
    vec![
        PluginSpec::new("launcher-logger").required(),
        PluginSpec::new("core-lists").required(),
        PluginSpec::new("version-store").required(),
        PluginSpec::new("settings").required(),
        PluginSpec::new("accounts").required(),
        PluginSpec::new("news"),
        PluginSpec::new("java-runtime").required(),
        PluginSpec::new("msa-login").required(),
        PluginSpec::new("launch").required(),
        // 版本管理排在本地内容**前面**：`game-content` 要在 apply 时拿到
        // `DownloadManager`（装模组更新要用）。它不做 inject，所以停用版本管理
        // 不会连带把内容页弄挂——只是那时拿不到下载器。
        PluginSpec::new("version-manager"),
        PluginSpec::new("game-content").required(),
        PluginSpec::new("game-launch").required(),
        PluginSpec::new("save-manager").required(),
        PluginSpec::new("mod-download"),
    ]
}

/// 降级组合：由 `presets/minimal.patch.yml` 表达，而不是复制一份清单。
pub const MINIMAL_PATCH: &str = include_str!("../../../presets/minimal.patch.yml");

pub fn minimal_preset() -> Vec<PluginSpec> {
    let base = standard_preset();
    let ops = parse_patch(MINIMAL_PATCH).expect("内置 minimal 补丁必须可解析");
    apply_patches(&base, &ops).expect("内置 minimal 补丁必须可应用")
}

/// 插件管理页需要的一段描述。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub state: PluginState,
    /// 是否是必需插件（界面据此把开关置灰；权威判断仍在内核）。
    pub required: bool,
}

fn meta(id: &str) -> (&'static str, &'static str) {
    match id {
        "launcher-logger" => ("启动器日志", "记录启动、下载与插件操作，提供本地排障日志"),
        "core-lists" => ("文件夹列表", "侧栏文件夹区的兜底实现：只读选择与打开目录"),
        "version-store" => ("版本扫描", "扫描 versions/*，解析加载器类型与就绪度"),
        "settings" => ("分层设置", "版本 > 文件夹 > 全局 > 内置默认 的配置解析"),
        "accounts" => ("账号", "全局账号模型：多个正版账号 + 单个离线档案"),
        "news" => (
            "新闻",
            "侧栏新闻模块；关闭后不再占位，下载队列会填满右侧空间",
        ),
        "launch" => (
            "启动与参数管理",
            "合并 inheritsFrom、过滤 rules、生成启动命令行",
        ),
        "java-runtime" => (
            "Java 运行时",
            "发现本机 JDK/JRE（注册表、安装目录、官方启动器、PATH）并按版本要求挑选",
        ),
        "game-content" => (
            "本地内容",
            "读取并管理模组 / 资源包 / 存档 / 日志，位置跟随版本隔离",
        ),
        "msa-login" => (
            "微软登录",
            "Microsoft OAuth2 设备码登录（Xbox Live → XSTS → Minecraft），令牌用 DPAPI 加密保存",
        ),
        "game-launch" => (
            "启动游戏",
            "真正 spawn Java、收集控制台、崩溃关键词分析并导出排障包",
        ),
        "save-manager" => (
            "存档管理",
            "列出与删除存档，查看/导入/删除存档的数据包（启用状态只读 level.dat）",
        ),
        "version-manager" => ("版本管理", "获取新版本：官方源 + BMCLAPI，可被其它插件覆盖"),
        "mod-download" => (
            "模组下载",
            "Modrinth + CurseForge 搜索、详情、前置与下载",
        ),
        _ => ("插件", ""),
    }
}

pub fn describe_plugins(kernel: &Kernel) -> Vec<PluginDescriptor> {
    let states = kernel.context().plugin_states();
    let specs = kernel.specs();
    let mut out = Vec::new();
    for spec in &specs {
        let (name, description) = meta(&spec.id);
        // 未启用的规格根本不会 apply，因此状态直接报 Disabled，
        // 而不是从状态表里拿一个永久的 Registered。
        let state = if spec.enabled {
            states
                .get(&spec.id)
                .copied()
                .unwrap_or(PluginState::Registered)
        } else {
            PluginState::Disabled
        };
        out.push(PluginDescriptor {
            id: spec.id.clone(),
            name: name.to_string(),
            description: description.to_string(),
            enabled: spec.enabled,
            state,
            required: spec.required,
        });
    }
    out
}

/// 便捷：把"取某个服务"写短一点。
pub fn roots_of(ctx: &Context) -> Option<Arc<RootRegistry>> {
    ctx.service::<RootRegistry>()
}

pub fn settings_of(ctx: &Context) -> Option<Arc<Settings>> {
    ctx.service::<Settings>()
}

pub fn accounts_of(ctx: &Context) -> Option<Arc<Accounts>> {
    ctx.service::<Accounts>()
}

pub fn versions_of(ctx: &Context) -> Option<Arc<VersionStore>> {
    ctx.service::<VersionStore>()
}

pub fn planner_of(ctx: &Context) -> Option<Arc<LaunchPlanner>> {
    ctx.service::<LaunchPlanner>()
}

/// 收集所有插件贡献的下载源，按 priority 从高到低。
pub fn all_install_sources(ctx: &Context) -> Vec<Arc<dyn InstallSourceApi>> {
    let mut merged: Vec<Arc<dyn InstallSourceApi>> = Vec::new();
    for table in ctx.services::<InstallSourceTable>() {
        for source in table.all() {
            if !merged.iter().any(|s| s.id() == source.id()) {
                merged.push(source);
            }
        }
    }
    merged.sort_by_key(|s| std::cmp::Reverse(s.priority()));
    merged
}

/// 读一下设置服务是否就绪（供 CLI 快速自检）。
pub fn settings_ready(ctx: &Context) -> bool {
    settings_of(ctx)
        .map(|s| s.global().is_ok())
        .unwrap_or(false)
}

/// 文件夹服务的便捷封装（给 CLI 用）。
pub fn selected_root(ctx: &Context) -> Option<kaleid_api::Root> {
    roots_of(ctx).and_then(|r| r.selected().ok().flatten())
}

pub fn root_by_alias_or_id(ctx: &Context, key: &str) -> Option<kaleid_api::Root> {
    let registry = roots_of(ctx)?;
    let roots = registry.list().ok()?;
    roots
        .into_iter()
        .find(|r| r.id == key || r.alias == key || r.path == key)
}

#[allow(dead_code)]
fn unused(_: &dyn SettingsServiceApi, _: &dyn RootRegistryService) {}
