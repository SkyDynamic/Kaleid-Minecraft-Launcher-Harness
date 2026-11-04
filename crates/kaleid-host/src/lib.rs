//! 宿主命令层。
//!
//! 界面只说"命令名 + JSON 参数"，这一层负责把它们翻译成插件服务调用。
//! **刻意不依赖 Tauri**：命令表可以脱离 WebView 单测，Tauri 外壳只做一层转发，
//! 将来换掉外壳（或加一条 CLI/IPC 通道）不用重写业务。

use std::collections::HashSet;
use std::path::Path;
use std::sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc, Mutex};
use std::time::Instant;

use serde::Deserialize;
use serde_json::{json, Value};

use kaleid_api::{
    Account, AccountServiceApi, ContentKind, ContentServiceApi, InstallRequest, IsolationMode,
    JavaServiceApi, ModDetailQuery, ModSearchQuery, ModSourceFilter, MsaAuthApi,
    RootRegistryService, SettingsServiceApi, VersionStoreService,
};
use kaleid_core_plugins::{
    all_install_sources, describe_plugins, minimal_preset, mod_types, register_all,
    skin::resolve_skin, standard_preset, Accounts, ContentService, DownloadManager, InstallService,
    JavaService, LaunchPlanner, ModInstallRequest, ModRankingQuery, ModService, MsaAuth,
    RootRegistry, Settings, GameProcessManager, SaveManager, VersionAdmin, VersionStore,
    LOG_TAIL_DEFAULT,
};
use kaleid_kernel::{DiagnosticRecord, Kernel, Severity};

// A finite, producer-owned catalog. Never persist an arbitrary incoming command name.
fn command_route(command: &str) -> (&'static str, &'static str) {
    const ROUTES: &[(&str, &str)] = &[
        ("roots_list", "core-lists"), ("roots_add", "core-lists"),
        ("roots_remove", "core-lists"), ("roots_rename", "core-lists"),
        ("roots_select", "core-lists"), ("roots_set_default", "core-lists"),
        ("roots_default", "core-lists"), ("roots_clear_default", "core-lists"),
        ("roots_set_isolation", "core-lists"), ("versions_scan", "version-store"),
        ("versions_verify", "version-store"), ("versions_delete", "version-manager"),
        ("versions_loader_status", "version-manager"), ("versions_update_loader", "version-manager"),
        ("versions_remote", "version-manager"), ("versions_check_name", "version-manager"),
        ("versions_install", "version-manager"), ("loaders_versions", "version-manager"),
        ("loader_version_times", "version-manager"), ("fabric_api_latest", "version-manager"),
        ("downloads_snapshot", "version-manager"), ("downloads_clear_finished", "version-manager"),
        ("downloads_cancel", "version-manager"), ("sources_list", "version-manager"),
        ("settings_global", "settings"), ("jvm_default_args", "settings"),
        ("settings_patch_global", "settings"), ("settings_version", "settings"),
        ("settings_patch_version", "settings"), ("settings_effective", "settings"),
        ("app_platform", "host"), ("skin_resolve", "accounts"),
        ("accounts_list", "accounts"), ("accounts_active", "accounts"),
        ("accounts_set_active", "accounts"), ("accounts_apply_offline", "accounts"),
        ("accounts_remove", "accounts"), ("msa_config", "msa-login"),
        ("msa_start", "msa-login"), ("msa_poll", "msa-login"),
        ("msa_cancel", "msa-login"), ("msa_states", "msa-login"),
        ("plugins_list", "kernel"), ("plugins_set_enabled", "kernel"),
        ("plugins_slots", "manifests"), ("plugins_manifests", "manifests"),
        ("launch_plan", "launch"), ("launch_command", "launch"),
        ("launch_check", "launch"), ("launch_start", "game-launch"),
        ("launch_state", "game-launch"), ("launch_states", "game-launch"),
        ("launch_stop", "game-launch"), ("launch_cancel", "game-launch"),
        ("launch_console_tail", "game-launch"), ("crash_report", "game-launch"),
        ("crash_export", "game-launch"), ("crash_export_zip", "game-launch"),
        ("saves_list", "save-manager"), ("saves_delete", "save-manager"),
        ("saves_datapacks_list", "save-manager"), ("saves_datapack_import", "save-manager"),
        ("saves_datapack_delete", "save-manager"), ("mods_target", "mod-download"),
        ("mods_types", "mod-download"), ("mods_ranking", "mod-download"),
        ("mods_search", "mod-download"), ("mods_detail", "mod-download"),
        ("mods_install", "mod-download"), ("mods_updates_check", "game-content"),
        ("mods_apply_update", "game-content"), ("content_dirs", "game-content"),
        ("content_mods_list", "game-content"), ("content_mods_set_enabled", "game-content"),
        ("content_mod_icon", "game-content"), ("content_packs_list", "game-content"),
        ("content_saves_list", "game-content"), ("content_logs_list", "game-content"),
        ("content_logs_read", "game-content"), ("content_import", "game-content"),
        ("content_delete", "game-content"), ("java_list", "java-runtime"),
        ("java_inspect", "java-runtime"), ("java_pick", "java-runtime"),
    ];
    ROUTES.iter().copied().find(|(name, _)| *name == command).unwrap_or(("unknown", "host"))
}

#[derive(Default)]
struct CommandDiagnostics {
    poll_failures: Mutex<HashSet<(&'static str, &'static str)>>,
    shutdown_started: AtomicBool,
}

fn command_diagnostics(kernel: &Kernel) -> Arc<CommandDiagnostics> {
    // Only initialization is process-wide; the bounded failure set is owned by this kernel.
    static INIT: Mutex<()> = Mutex::new(());
    if let Some(state) = kernel.context().service::<CommandDiagnostics>() { return state; }
    let _guard = INIT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = kernel.context().service::<CommandDiagnostics>() { return state; }
    kernel.context().provide_root("host-command-diagnostics", CommandDiagnostics::default(), 0);
    kernel.context().service::<CommandDiagnostics>().unwrap_or_else(|| Arc::new(CommandDiagnostics::default()))
}

fn uuid_id(value: &str) -> bool {
    value.len() == 36 && value.bytes().enumerate().all(|(i, b)| {
        if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() }
    })
}

fn public_version_id(value: &str) -> bool {
    if value.len() > 32 { return false; }
    let parts: Vec<_> = value.split('.').collect();
    if (2..=3).contains(&parts.len()) && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return value.len() <= 32;
    }
    let b = value.as_bytes();
    b.len() == 6 && b[2] == b'w' && b[5].is_ascii_lowercase()
        && [b[0], b[1], b[3], b[4]].iter().all(|b| b.is_ascii_digit())
}

#[derive(Default)]
struct CommandIds {
    root: Option<String>,
    version: Option<String>,
    session: Option<String>,
    job: Option<String>,
}

impl CommandIds {
    fn from_args(command: &str, args: &Value) -> Self {
        let mut ids = Self::default();
        // Explicit command + field roles. Account IDs/names and custom version names are omitted.
        let root_key = match command {
            "roots_remove" | "roots_rename" | "roots_select" | "roots_set_default" | "roots_set_isolation" => Some("id"),
            "versions_scan" | "versions_verify" | "versions_delete" | "versions_loader_status" |
            "versions_update_loader" | "versions_install" | "versions_check_name" |
            "settings_version" | "settings_patch_version" | "settings_effective" |
            "launch_plan" | "launch_command" | "launch_check" | "launch_start" |
            "saves_list" | "saves_delete" | "saves_datapacks_list" | "saves_datapack_import" |
            "saves_datapack_delete" | "mods_target" | "mods_detail" | "mods_install" |
            "mods_updates_check" | "mods_apply_update" | "content_dirs" | "content_mods_list" |
            "content_mods_set_enabled" | "content_mod_icon" | "content_packs_list" |
            "content_saves_list" | "content_logs_list" | "content_logs_read" | "content_import" |
            "content_delete" => Some("rootId"),
            _ => None,
        };
        if let Some(key) = root_key {
            ids.root = args.get(key).and_then(Value::as_str).filter(|id| uuid_id(id)).map(str::to_owned);
            ids.version = args.get("versionId").and_then(Value::as_str).filter(|id| public_version_id(id)).map(str::to_owned);
        }
        if matches!(command, "launch_stop" | "launch_cancel" | "launch_console_tail" | "crash_report" | "crash_export" | "crash_export_zip") {
            ids.session = args.get("sessionId").and_then(Value::as_u64).map(|id| id.to_string());
        }
        if command == "downloads_cancel" {
            ids.job = args.get("id").and_then(Value::as_str).filter(|id| uuid_id(id)).map(str::to_owned);
        }
        ids
    }

    fn attach(&self, mut record: DiagnosticRecord) -> DiagnosticRecord {
        if let Some(id) = &self.root { record = record.with_field("root_id", id.clone()); }
        if let Some(id) = &self.version { record = record.with_field("version_id", id.clone()); }
        if let Some(id) = &self.session { record = record.with_session_id(id.clone()); }
        if let Some(id) = &self.job { record = record.with_job_id(id.clone()); }
        record
    }
}

fn observe_command(
    kernel: &Kernel,
    command: &str,
    args: Value,
    execute: impl FnOnce(Value) -> Result<Value, String>,
) -> Result<Value, String> {
    static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);
    let (command, component) = command_route(command);
    let operation = format!("{}-{}", std::process::id(), NEXT_OPERATION.fetch_add(1, Ordering::Relaxed));
    let started = Instant::now();
    let mut ids = CommandIds::from_args(command, &args);
    let polling = matches!(command, "downloads_snapshot" | "launch_state" | "launch_states" | "launch_console_tail" | "msa_states" | "msa_poll");
    let record = |event, severity| DiagnosticRecord::new(component, event, severity)
        .with_operation_id(operation.clone()).with_field("command", command).with_field("source", "host");
    if !polling { kernel.context().diagnostic(ids.attach(record("command.start", Severity::Info)).with_field("outcome", "started")); }
    let result = execute(args);
    if let Ok(value) = &result {
        // Only these typed, generated result IDs cross the boundary; never persist the body.
        if matches!(command, "versions_install" | "versions_update_loader" | "mods_install") {
            ids.job = value.get("jobId").and_then(Value::as_str).filter(|id| uuid_id(id)).map(str::to_owned);
        }
        if command == "launch_start" {
            ids.session = value.get("sessionId").and_then(Value::as_u64).map(|id| id.to_string());
        }
        if command == "roots_add" {
            ids.root = value.get("id").and_then(Value::as_str).filter(|id| uuid_id(id)).map(str::to_owned);
        }
    }
    let (outcome, category) = match &result {
        Err(_) => ("failed", if command == "unknown" { "unknown_command" } else { "command_failed" }),
        Ok(value) if command == "launch_check" && value.get("ok").and_then(Value::as_bool) == Some(false) => {
            let category = match value.pointer("/blocks/0/kind").and_then(Value::as_str) {
                Some("no-account") => "no-account", Some("java") => "java", Some("version") => "version", Some("auth") => "auth", _ => "blocked",
            };
            ("blocked", category)
        }
        Ok(_) if matches!(command, "versions_install" | "versions_update_loader" | "mods_install" | "launch_start") => ("accepted", "queued"),
        Ok(_) => ("succeeded", "completed"),
    };
    if polling {
        let state = command_diagnostics(kernel);
        let mut failures = state.poll_failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if result.is_ok() {
            failures.retain(|(name, _)| *name != command);
            return result;
        }
        if !failures.insert((command, category)) { return result; }
    }
    kernel.context().diagnostic(ids.attach(record("command.result", if outcome == "failed" { Severity::Warn } else { Severity::Info }))
        .with_field("outcome", outcome).with_field("category", category)
        .with_field("duration_ms", started.elapsed().as_millis().min(u64::MAX as u128) as u64));
    result
}

fn lifecycle(kernel: &Kernel, event: &'static str, outcome: &'static str, category: &'static str) {
    kernel.context().diagnostic(DiagnosticRecord::new("host", event, if matches!(outcome, "failed" | "rejected") { Severity::Warn } else { Severity::Info })
        .with_field("outcome", outcome).with_field("category", category));
}

fn finish_logging(kernel: &Kernel) {
    let state = command_diagnostics(kernel);
    if state.shutdown_started.swap(true, Ordering::AcqRel) { return; }
    lifecycle(kernel, "launcher.shutdown", "started", "logger_only");
    // Existing writer-owned cleanup removes the subscriber, drains and flushes once.
    // Removing its effects also prevents a later Kernel Drop from waiting again.
    kernel.context().dispose_owner("launcher-logger");
}

/// 宿主状态：一个进程一套内核。
pub struct Host {
    kernel: Kernel,
    minimal: bool,
    data_root: std::path::PathBuf,
}

impl Host {
    /// 启动内核并装载组合。`minimal = true` 用于降级矩阵回归。
    pub fn boot(data_root: &Path, minimal: bool) -> Result<Self, String> {
        let kernel = Kernel::new();
        register_all(&kernel, data_root);
        let _ = kaleid_core_plugins::bootstrap_logger(&kernel, data_root);
        let _ = command_diagnostics(&kernel);
        // Invalid summaries contain untrusted names, paths and errors: persist none of them.
        for _ in kaleid_core_plugins::list_manifests(data_root).into_iter().filter(|m| m.error.is_some()) {
            lifecycle(&kernel, "manifest.validation", "rejected", "invalid_manifest");
        }
        // 第三方清单：配置类动作先改规格，服务类动作等内核就绪后再注册。
        let manifests = kaleid_core_plugins::load_ready_manifests(data_root);
        let mut specs = if minimal {
            minimal_preset()
        } else {
            standard_preset()
        };
        if let Err(error) = kaleid_core_plugins::apply_to_specs(&manifests, &mut specs) {
            lifecycle(&kernel, "manifest.apply", "rejected", "apply_rejected");
            eprintln!("清单未生效：{error}");
        }
        kaleid_core_plugins::ensure_logger_spec(&mut specs);
        if let Err(error) = kernel.load(&specs) {
            lifecycle(&kernel, "startup.result", "failed", "kernel_load_failed");
            // Run writer-owned drain/flush once; later Kernel Drop cannot repeat its wait.
            finish_logging(&kernel);
            return Err(error.to_string());
        }
        for manifest in &manifests {
            if let Some((owner, sources)) = kaleid_core_plugins::manifest_source_table(manifest) {
                kernel.context().provide(
                    "install-sources",
                    kaleid_core_plugins::InstallSourceTable { owner, sources },
                    0,
                );
            }
        }
        apply_manifest_defaults(data_root, &kernel, &manifests);
        lifecycle(&kernel, "startup.result", "succeeded", "ready");
        Ok(Host {
            kernel,
            minimal,
            data_root: data_root.to_path_buf(),
        })
    }

    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    pub fn is_minimal(&self) -> bool {
        self.minimal
    }

    /// Drain launcher diagnostics once, with the writer's single bounded deadline.
    /// This does not dispose business services, wait for workers, or stop child games.
    pub fn shutdown_logging(&self) {
        finish_logging(&self.kernel);
    }

    /// 执行一条命令。
    pub fn dispatch(&self, cmd: &str, args: Value) -> Result<Value, String> {
        // 清单相关的两条命令要读数据目录，所以留在宿主层，不进无状态命令表。
        match cmd {
            "plugins_slots" => {
                return observe_command(&self.kernel, cmd, args, |_| {
                    let manifests = kaleid_core_plugins::load_ready_manifests(&self.data_root);
                    to_value(kaleid_core_plugins::manifest_slots(&manifests))
                });
            }
            "plugins_manifests" => {
                return observe_command(&self.kernel, cmd, args, |_| to_value(kaleid_core_plugins::list_manifests(&self.data_root)));
            }
            _ => {}
        }
        dispatch(&self.kernel, cmd, args)
    }
}

/// 清单声明的默认值只在用户没有显式设置时生效。
///
/// 「显式设置」看的是 `settings.json` 里的**原始键**：把 GlobalSettings 序列化一遍
/// 会让所有字段都出现（结构体是满的），那样默认值永远不会生效。
fn apply_manifest_defaults(
    data_root: &Path,
    kernel: &Kernel,
    manifests: &[kaleid_core_plugins::Manifest],
) {
    let Some(settings) = kernel.context().service::<Settings>() else {
        return;
    };
    let stored: serde_json::Map<String, Value> =
        std::fs::read_to_string(kaleid_core_plugins::paths::settings_file(data_root))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
    for (key, value) in kaleid_core_plugins::default_settings(manifests) {
        if stored.contains_key(&key) {
            continue;
        }
        let mut patch = serde_json::Map::new();
        patch.insert(key.clone(), value);
        if let Err(error) = settings.patch_global(&Value::Object(patch)) {
            lifecycle(kernel, "manifest.default", "rejected", "default_rejected");
            eprintln!("清单默认值 {key} 未生效：{error}");
        }
    }
}

fn roots(kernel: &Kernel) -> Result<std::sync::Arc<RootRegistry>, String> {
    kernel
        .context()
        .service::<RootRegistry>()
        .ok_or_else(|| "文件夹服务不可用".to_string())
}

fn settings(kernel: &Kernel) -> Result<std::sync::Arc<Settings>, String> {
    kernel
        .context()
        .service::<Settings>()
        .ok_or_else(|| "设置服务不可用".to_string())
}

fn accounts(kernel: &Kernel) -> Result<std::sync::Arc<Accounts>, String> {
    kernel
        .context()
        .service::<Accounts>()
        .ok_or_else(|| "账号服务不可用".to_string())
}

fn versions(kernel: &Kernel) -> Result<std::sync::Arc<VersionStore>, String> {
    kernel
        .context()
        .service::<VersionStore>()
        .ok_or_else(|| "版本扫描服务不可用".to_string())
}

fn planner(kernel: &Kernel) -> Result<std::sync::Arc<LaunchPlanner>, String> {
    kernel
        .context()
        .service::<LaunchPlanner>()
        .ok_or_else(|| "启动服务不可用".to_string())
}

fn downloads(kernel: &Kernel) -> Result<std::sync::Arc<DownloadManager>, String> {
    kernel
        .context()
        .service::<DownloadManager>()
        .ok_or_else(|| "下载服务不可用（版本管理插件可能已停用）".to_string())
}

fn install(kernel: &Kernel) -> Result<std::sync::Arc<InstallService>, String> {
    kernel
        .context()
        .service::<InstallService>()
        .ok_or_else(|| "安装服务不可用（版本管理插件可能已停用）".to_string())
}

fn version_admin(kernel: &Kernel) -> Result<std::sync::Arc<VersionAdmin>, String> {
    kernel
        .context()
        .service::<VersionAdmin>()
        .ok_or_else(|| "版本管理服务不可用（版本管理插件可能已停用）".to_string())
}

/// 真实启动的运行时（进程、控制台、崩溃报告都挂在它上面）。
fn game(kernel: &Kernel) -> Result<std::sync::Arc<GameProcessManager>, String> {
    kernel
        .context()
        .service::<GameProcessManager>()
        .ok_or_else(|| "游戏启动服务不可用（game-launch 插件可能已停用）".to_string())
}

fn saves(kernel: &Kernel) -> Result<std::sync::Arc<SaveManager>, String> {
    kernel
        .context()
        .service::<SaveManager>()
        .ok_or_else(|| "存档管理不可用（save-manager 插件可能已停用）".to_string())
}

/// 存档/模组这些「跟着版本走」的目录：隔离开启时是版本自己的目录。
fn game_dir_of(
    kernel: &Kernel,
    root: &kaleid_api::Root,
    version_id: &str,
) -> Result<String, String> {
    let settings = settings(kernel)?;
    let roots_svc = roots(kernel)?;
    let (_root, dir, _isolated, _from) =
        kaleid_core_plugins::resolve_game_dir(&*roots_svc, &*settings, &root.id, version_id)
            .map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().to_string())
}

/// `mods/` 里的 jar 文件名（崩溃分析要把日志里的名字对回文件）。
fn mods_in(game_dir: &str) -> Vec<String> {
    std::fs::read_dir(std::path::Path::new(game_dir).join("mods"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.to_ascii_lowercase().ends_with(".jar"))
                .collect()
        })
        .unwrap_or_default()
}

fn mods(kernel: &Kernel) -> Result<std::sync::Arc<ModService>, String> {
    kernel
        .context()
        .service::<ModService>()
        .ok_or_else(|| "模组下载服务不可用（插件可能已停用）".to_string())
}

fn content(kernel: &Kernel) -> Result<std::sync::Arc<ContentService>, String> {
    kernel
        .context()
        .service::<ContentService>()
        .ok_or_else(|| "本地内容服务不可用（game-content 插件可能已停用）".to_string())
}

fn java(kernel: &Kernel) -> Result<std::sync::Arc<JavaService>, String> {
    kernel
        .context()
        .service::<JavaService>()
        .ok_or_else(|| "Java 服务不可用（java-runtime 插件可能已停用）".to_string())
}

fn msa(kernel: &Kernel) -> Result<std::sync::Arc<MsaAuth>, String> {
    kernel
        .context()
        .service::<MsaAuth>()
        .ok_or_else(|| "微软登录服务不可用（msa-login 插件可能已停用）".to_string())
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddArgs {
    path: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
struct IdArgs {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameArgs {
    id: String,
    alias: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IsolationArgs {
    id: String,
    mode: IsolationMode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RootIdArgs {
    root_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionArgs {
    root_id: String,
    version_id: String,
}

#[derive(Deserialize)]
struct PatchArgs {
    patch: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionPatchArgs {
    root_id: String,
    version_id: String,
    patch: Value,
}

#[derive(Deserialize)]
struct OfflineArgs {
    name: String,
}

#[derive(Deserialize)]
struct ToggleArgs {
    id: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteVersionsArgs {
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NameCheckArgs {
    root_id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoaderArgs {
    game_version: String,
    loader: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoaderTimesArgs {
    #[serde(default)]
    game_version: String,
    loader: String,
    versions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FabricApiArgs {
    game_version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveArgs {
    root_id: String,
    version_id: String,
    folder: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavePackArgs {
    root_id: String,
    version_id: String,
    folder: String,
    file_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveImportArgs {
    root_id: String,
    version_id: String,
    folder: String,
    /// 要导入的 .zip 绝对路径。
    source: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CrashExportArgs {
    #[serde(default)]
    dir: Option<String>,
    /// 导哪个会话的日志；不传 = 最近一个会话。
    #[serde(default)]
    session_id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CrashZipArgs {
    /// 用户选的导出目录（界面用系统选择器拿到）。
    target_dir: String,
    #[serde(default)]
    session_id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsoleTailArgs {
    #[serde(default)]
    session_id: Option<u64>,
    /// 最多读多少字节；不传走默认 256 KB。
    #[serde(default)]
    max_bytes: Option<usize>,
}

/// 只带一个（可选）会话 id 的参数：`launch_stop` / `crash_report` 共用。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionArgs {
    /// 不传 = 作用于全部/最近一个会话（视命令而定）。
    #[serde(default)]
    session_id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoaderUpdateArgs {
    root_id: String,
    version_id: String,
    /// 留空 = 用推荐/最新版本。
    #[serde(default)]
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallArgs {
    root_id: String,
    game_version: String,
    version_name: String,
    loader: kaleid_api::InstallLoader,
    #[serde(default)]
    install_fabric_api: bool,
    #[serde(default)]
    optifine: Option<kaleid_api::OptifineRef>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkinArgs {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("参数不合法：{e}"))
}

// ---- 模组下载 ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModTargetArgs {
    root_id: String,
    version_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModRankingArgs {
    #[serde(default)]
    force: bool,
    /// 类型表里的 id；空 = 不按分类筛。
    #[serde(default)]
    type_id: Option<String>,
    /// 显式指定游戏版本 / 加载器。**只有筛选条会给**：绝不由当前选中版本推出
    /// （那条老路会让"整合包名字当版本"的目录把整页排行筛空）。
    /// 老调用传的 `rootId` / `versionId` 现在被 serde 直接忽略。
    #[serde(default)]
    game_version: Option<String>,
    #[serde(default)]
    loader: Option<String>,
    /// 0 起的页码，一页 50 个。
    #[serde(default)]
    page: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModSearchArgs {
    #[serde(default)]
    query: String,
    #[serde(default)]
    source: ModSourceFilter,
    #[serde(default)]
    type_id: Option<String>,
    #[serde(default)]
    page_size: Option<u32>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    latest_game_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModDetailArgs {
    source: kaleid_api::ModSource,
    project_id: String,
    #[serde(default)]
    root_id: Option<String>,
    #[serde(default)]
    version_id: Option<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModApplyUpdateArgs {
    root_id: String,
    version_id: String,
    /// 就是 `mods_updates_check` 返回的 `updates[]` 里的一条。
    update: kaleid_api::ModUpdate,
}

// ---- 本地内容 ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentArgs {
    root_id: String,
    version_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentToggleArgs {
    root_id: String,
    version_id: String,
    file: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentFileArgs {
    root_id: String,
    version_id: String,
    kind: ContentKind,
    file: String,
}

/// 取某个模组的图标：`fileName` 是列表里的真实文件名（可能是 `.jar.disabled`）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModIconArgs {
    root_id: String,
    version_id: String,
    file_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentImportArgs {
    root_id: String,
    version_id: String,
    kind: ContentKind,
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentLogArgs {
    root_id: String,
    version_id: String,
    file: String,
    #[serde(default)]
    max_bytes: Option<u64>,
}

// ---- Java 运行时 ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JavaInspectArgs {
    path: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct JavaPickArgs {
    #[serde(default)]
    major: Option<u32>,
    /// 手动指定的路径（版本级或全局设置里的）；非空时只用它。
    #[serde(default)]
    java_path: Option<String>,
}

// ---------------------------------------------------------------------------
// 命令表
// ---------------------------------------------------------------------------

/// 命令名 → 实现。界面侧的命令常量必须与此表一致。
pub fn dispatch(kernel: &Kernel, cmd: &str, args: Value) -> Result<Value, String> {
    observe_command(kernel, cmd, args, |args| dispatch_inner(kernel, cmd, args))
}

fn dispatch_inner(kernel: &Kernel, cmd: &str, args: Value) -> Result<Value, String> {
    let ctx = kernel.context();
    match cmd {
        // ---- 文件夹 ----
        "roots_list" => to_value(roots(kernel)?.list().map_err(|e| e.to_string())?),
        "roots_add" => {
            let a: AddArgs = parse(args)?;
            let root = roots(kernel)?
                .add(Path::new(&a.path), a.alias.as_deref(), a.force)
                .map_err(|e| e.to_string())?;
            let _ = roots(kernel)?.select(&root.id);
            to_value(root)
        }
        "roots_remove" => {
            let a: IdArgs = parse(args)?;
            roots(kernel)?.remove(&a.id).map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "roots_rename" => {
            let a: RenameArgs = parse(args)?;
            to_value(
                roots(kernel)?
                    .rename(&a.id, &a.alias)
                    .map_err(|e| e.to_string())?,
            )
        }
        "roots_select" => {
            let a: IdArgs = parse(args)?;
            to_value(roots(kernel)?.select(&a.id).map_err(|e| e.to_string())?)
        }
        "roots_set_default" => {
            let a: IdArgs = parse(args)?;
            to_value(
                roots(kernel)?
                    .set_default(&a.id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "roots_default" => to_value(
            roots(kernel)?
                .default_root()
                .map_err(|e| e.to_string())?,
        ),
        "roots_clear_default" => {
            roots(kernel)?
                .clear_default()
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "roots_set_isolation" => {
            let a: IsolationArgs = parse(args)?;
            to_value(
                roots(kernel)?
                    .set_isolation(&a.id, a.mode)
                    .map_err(|e| e.to_string())?,
            )
        }

        // ---- 版本 ----
        "versions_scan" => {
            let a: RootIdArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            to_value(
                versions(kernel)?
                    .scan(Path::new(&root.path))
                    .map_err(|e| e.to_string())?,
            )
        }
        "versions_delete" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            to_value(
                version_admin(kernel)?
                    .delete_version(&root, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "versions_loader_status" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            to_value(
                version_admin(kernel)?
                    .loader_status(&root, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "versions_update_loader" => {
            let a: LoaderUpdateArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            // 「装哪个版本」由版本管理算（推荐版优先），真正的下载/落盘仍在安装服务里，
            // 这样更新与全新安装共用同一套目录布局。
            let (loader, _game, target) = version_admin(kernel)?
                .resolve_update_target(&root, &a.version_id, a.version.as_deref())
                .map_err(|e| e.to_string())?;
            to_value(
                install(kernel)?
                    .update_loader(root, &a.version_id, loader, &target)
                    .map_err(|e| e.to_string())?,
            )
        }
        "skin_resolve" => {
            let a: SkinArgs = parse(args)?;
            to_value(
                resolve_skin(a.name.as_deref(), a.uuid.as_deref(), a.url.as_deref())
                    .map_err(|e| e.to_string())?,
            )
        }
        "versions_remote" => {
            let a: RemoteVersionsArgs = parse(args)?;
            to_value(
                install(kernel)?
                    .remote_versions(a.force)
                    .map_err(|e| e.to_string())?,
            )
        }
        "versions_check_name" => {
            let a: NameCheckArgs = parse(args)?;
            to_value(
                install(kernel)?
                    .check_name(&a.root_id, &a.name)
                    .map_err(|e| e.to_string())?,
            )
        }
        "versions_install" => {
            let a: InstallArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let req = InstallRequest {
                root_id: a.root_id,
                game_version: a.game_version,
                version_name: a.version_name,
                loader: a.loader,
                install_fabric_api: a.install_fabric_api,
                optifine: a.optifine,
            };
            to_value(
                install(kernel)?
                    .enqueue(root, req)
                    .map_err(|e| e.to_string())?,
            )
        }
        "loaders_versions" => {
            let a: LoaderArgs = parse(args)?;
            to_value(
                install(kernel)?
                    .loader_versions(&a.game_version, &a.loader)
                    .map_err(|e| e.to_string())?,
            )
        }
        "loader_version_times" => {
            let a: LoaderTimesArgs = parse(args)?;
            to_value(
                install(kernel)?
                    .loader_times(&a.game_version, &a.loader, &a.versions)
                    .map_err(|e| e.to_string())?,
            )
        }
        "fabric_api_latest" => {
            let a: FabricApiArgs = parse(args)?;
            to_value(
                install(kernel)?
                    .fabric_api_latest(&a.game_version)
                    .map_err(|e| e.to_string())?,
            )
        }

        // ---- 下载队列 ----
        "downloads_snapshot" => to_value(downloads(kernel)?.snapshot()),
        "downloads_clear_finished" => {
            downloads(kernel)?.clear_finished();
            Ok(Value::Null)
        }
        "downloads_cancel" => {
            let a: IdArgs = parse(args)?;
            downloads(kernel)?
                .cancel(&a.id)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }

        // ---- 设置 ----
        "settings_global" => to_value(settings(kernel)?.global().map_err(|e| e.to_string())?),
        // 全局 JVM 参数栏的「恢复默认」按这里的内容填回去。
        // 清单只存在 Rust 一侧（`kaleid_api::DEFAULT_JVM_ARGS`），界面不另抄一份，
        // 否则哪天改了参数、界面还在"恢复"旧的那套。
        "jvm_default_args" => to_value(kaleid_api::default_jvm_args()),
        // 界面要知道自己在哪个系统上：有的开关只在 Windows 有意义
        // （「使用高性能显卡」写的是 Windows 的注册表项）。
        "app_platform" => to_value(serde_json::json!({
            "os": std::env::consts::OS,
            "highPerformanceGpuSupported": kaleid_api::HIGH_PERFORMANCE_GPU_SUPPORTED,
        })),
        "settings_patch_global" => {
            let a: PatchArgs = parse(args)?;
            let next = settings(kernel)?
                .patch_global(&a.patch)
                .map_err(|e| e.to_string())?;
            // 「使用高性能显卡」一改就生效：写入/删除注册表只影响启动器自己，
            // Java 那一条在启动时按同样的开关写。失败只是警告，不拦住设置保存。
            if let Some(enabled) = a.patch.get("highPerformanceGpu").and_then(|v| v.as_bool()) {
                let _ = kaleid_core_plugins::gpu::apply_launcher_preference(enabled);
            }
            to_value(next)
        }
        "settings_version" => {
            let a: VersionArgs = parse(args)?;
            to_value(
                settings(kernel)?
                    .version_settings(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "settings_patch_version" => {
            let a: VersionPatchArgs = parse(args)?;
            to_value(
                settings(kernel)?
                    .patch_version(&a.root_id, &a.version_id, &a.patch)
                    .map_err(|e| e.to_string())?,
            )
        }
        "settings_effective" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            to_value(
                settings(kernel)?
                    .resolve(&a.root_id, &a.version_id, root.isolation)
                    .map_err(|e| e.to_string())?,
            )
        }

        // ---- 账号 ----
        "accounts_list" => to_value(accounts(kernel)?.list().map_err(|e| e.to_string())?),
        "accounts_active" => to_value(accounts(kernel)?.active().map_err(|e| e.to_string())?),
        "accounts_set_active" => {
            let a: IdArgs = parse(args)?;
            to_value(
                accounts(kernel)?
                    .set_active(&a.id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "accounts_apply_offline" => {
            let a: OfflineArgs = parse(args)?;
            to_value(
                accounts(kernel)?
                    .apply_offline_name(&a.name)
                    .map_err(|e| e.to_string())?,
            )
        }
        "accounts_remove" => {
            let a: IdArgs = parse(args)?;
            accounts(kernel)?.remove(&a.id).map_err(|e| e.to_string())?;
            // 顺手清掉这个账号加密保存的令牌：账号列表里已经没有它了，
            // 留着只是把一份能用一年的刷新令牌忘在磁盘上。
            if let Ok(msa) = msa(kernel) {
                let _ = msa.sign_out(&a.id);
            }
            Ok(Value::Null)
        }

        // ---- 微软登录（设备码流程）----
        //
        // 分三步而不是一个阻塞调用：界面先 `start` 拿到用户码，然后按 `interval`
        // 反复 `poll`，中间可以随时 `cancel`。后端不睡眠，所以没有"点了取消但
        // 线程还在打接口"这种事。
        "msa_config" => to_value(msa(kernel)?.config()),
        "msa_start" => to_value(msa(kernel)?.start().map_err(|e| e.to_string())?),
        "msa_poll" => to_value(msa(kernel)?.poll().map_err(|e| e.to_string())?),
        "msa_cancel" => {
            msa(kernel)?.cancel().map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "msa_states" => to_value(msa(kernel)?.states().map_err(|e| e.to_string())?),

        // ---- 插件 ----
        "plugins_list" => to_value(describe_plugins(kernel)),
        "plugins_set_enabled" => {
            let a: ToggleArgs = parse(args)?;
            if a.enabled {
                kernel.enable(&a.id).map_err(|e| e.to_string())?;
            } else {
                kernel.disable(&a.id).map_err(|e| e.to_string())?;
            }
            to_value(describe_plugins(kernel))
        }

        // ---- 启动 ----
        "launch_plan" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let account: Account = accounts(kernel)?
                .active()
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "还没有账号，先在概览的登录卡里选一个".to_string())?;
            to_value(
                planner(kernel)?
                    .plan_for_root(&root, &a.version_id, &account)
                    .map_err(|e| e.to_string())?,
            )
        }
        // 给"启动游戏"按钮用：返回命令行字符串（真正 spawn 留待运行时插件）。
        "launch_command" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let account: Account = accounts(kernel)?
                .active()
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "还没有账号".to_string())?;
            let plan = planner(kernel)?
                .plan_for_root(&root, &a.version_id, &account)
                .map_err(|e| e.to_string())?;
            Ok(json!({ "commandLine": plan.command_line(), "plan": plan }))
        }

        // ---- 真实启动与崩溃（运行时） ----
        "launch_start" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let account: Account = accounts(kernel)?
                .active()
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "还没有账号，先在概览的登录卡里选一个".to_string())?;
            let plan = planner(kernel)?
                .plan_for_root(&root, &a.version_id, &account)
                .map_err(|e| e.to_string())?;
            // 令牌在这一步才取：计划快照里永远是占位值。
            let token = msa(kernel)?
                .access_token(&account.id)
                .map_err(|e| e.to_string())?;
            if account.kind == kaleid_api::AccountKind::Msa && token.is_none() {
                return Err("登录状态已失效，请重新登录这个微软账号".to_string());
            }
            let mods = mods_in(&plan.game_dir);
            to_value(
                game(kernel)?
                    .start(plan, token, mods)
                    .map_err(|e| e.to_string())?,
            )
        }
        "launch_state" => to_value(game(kernel)?.state()),
        // 多开：界面轮询的是"所有会话"，不是"那一个会话"。
        "launch_states" => to_value(game(kernel)?.states()),
        "launch_stop" => {
            let a: SessionArgs = parse(args)?;
            to_value(
                game(kernel)?
                    .stop(a.session_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        // 启动前检查：只读，不 spawn。登录失败要**弹原因并阻止启动**，
        // 所以界面先问这个，`ok == false` 就直接不开游戏。
        "launch_check" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            // 没有账号也算一条 block（不是命令错误）：界面照着它弹窗并给「去登录」。
            let Some(account) = accounts(kernel)?.active().map_err(|e| e.to_string())? else {
                return to_value(kaleid_api::LaunchCheck::blocked(kaleid_api::LaunchBlock {
                    kind: kaleid_api::LaunchBlockKind::NoAccount,
                    message: "还没有登录账号，先在概览的登录卡里选一个".to_string(),
                }));
            };
            let mut notes = Vec::new();
            // Java / 版本文件的问题由 planner 直接报出来；它同时也会把
            // 「缺哪些文件」讲清楚，比自己再实现一遍校验可靠。
            match planner(kernel)?.plan_for_root(&root, &a.version_id, &account) {
                Ok(plan) => notes.extend(plan.jvm_notes.clone()),
                Err(e) => {
                    let message = e.to_string();
                    let kind = if message.contains("Java") || message.contains("java") {
                        kaleid_api::LaunchBlockKind::Java
                    } else {
                        kaleid_api::LaunchBlockKind::Version
                    };
                    return to_value(kaleid_api::LaunchCheck::blocked(kaleid_api::LaunchBlock {
                        kind,
                        message,
                    }));
                }
            }
            match msa(kernel)?.access_token(&account.id) {
                Ok(Some(_)) => to_value(kaleid_api::LaunchCheck::pass(notes)),
                Ok(None) if account.kind == kaleid_api::AccountKind::Msa => {
                    to_value(kaleid_api::LaunchCheck::blocked(kaleid_api::LaunchBlock {
                        kind: kaleid_api::LaunchBlockKind::Auth,
                        message: "这个微软账号没有可用的登录令牌，请重新登录".to_string(),
                    }))
                }
                // 离线账号本来就没有令牌
                Ok(None) => to_value(kaleid_api::LaunchCheck::pass(notes)),
                Err(e) => {
                    to_value(kaleid_api::LaunchCheck::blocked(kaleid_api::LaunchBlock {
                        kind: kaleid_api::LaunchBlockKind::Auth,
                        message: e.to_string(),
                    }))
                }
            }
        }
        "crash_report" => {
            let a: SessionArgs = parse(args)?;
            to_value(game(kernel)?.report(a.session_id))
        }
        "crash_export" => {
            let a: CrashExportArgs = parse(args)?;
            to_value(
                game(kernel)?
                    .export(a.dir.as_deref().map(Path::new), a.session_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        // 打成 ZIP 落到用户选的目录：文件名 `崩溃日志-2026_10_31 20-45-01.zip`。
        "crash_export_zip" => {
            let a: CrashZipArgs = parse(args)?;
            to_value(
                game(kernel)?
                    .export_zip(Path::new(&a.target_dir), a.session_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        // 「本次日志」：读的是这次启动自己那份 console.log，不是游戏目录的 latest.log。
        "launch_console_tail" => {
            let a: ConsoleTailArgs = parse(args)?;
            let max = a
                .max_bytes
                .unwrap_or(kaleid_core_plugins::LOG_TAIL_DEFAULT)
                .min(kaleid_core_plugins::LOG_TAIL_MAX);
            to_value(
                game(kernel)?
                    .console_tail(a.session_id, max)
                    .map_err(|e| e.to_string())?,
            )
        }
        // 取消一次还在准备阶段的启动（PCL2 进度条上的「取消」）。
        "launch_cancel" => {
            let a: SessionArgs = parse(args)?;
            let session_id = a.session_id.ok_or_else(|| "缺少会话 id".to_string())?;
            to_value(
                game(kernel)?
                    .cancel(session_id)
                    .map_err(|e| e.to_string())?,
            )
        }

        // ---- 存档管理 ----
        "saves_list" => {
            let a: VersionArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let dir = game_dir_of(kernel, &root, &a.version_id)?;
            to_value(
                saves(kernel)?
                    .list(Path::new(&dir))
                    .map_err(|e| e.to_string())?,
            )
        }
        "saves_delete" => {
            let a: SaveArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let dir = game_dir_of(kernel, &root, &a.version_id)?;
            to_value(
                saves(kernel)?
                    .delete(Path::new(&dir), &a.folder)
                    .map_err(|e| e.to_string())?,
            )
        }
        "saves_datapacks_list" => {
            let a: SaveArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let dir = game_dir_of(kernel, &root, &a.version_id)?;
            to_value(
                saves(kernel)?
                    .datapacks(Path::new(&dir), &a.folder)
                    .map_err(|e| e.to_string())?,
            )
        }
        "saves_datapack_import" => {
            let a: SaveImportArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let dir = game_dir_of(kernel, &root, &a.version_id)?;
            to_value(
                saves(kernel)?
                    .import_datapack(Path::new(&dir), &a.folder, Path::new(&a.source))
                    .map_err(|e| e.to_string())?,
            )
        }
        "saves_datapack_delete" => {
            let a: SavePackArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let dir = game_dir_of(kernel, &root, &a.version_id)?;
            saves(kernel)?
                .delete_datapack(Path::new(&dir), &a.folder, &a.file_name)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }

        // ---- 模组下载 ----
        "mods_target" => {
            let a: ModTargetArgs = parse(args)?;
            to_value(
                mods(kernel)?
                    .target(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "mods_types" => to_value(mod_types()),
        "mods_ranking" => {
            let a: ModRankingArgs = parse(args)?;
            // 初始列表必须是**没有筛选条件**的下载量排行（PCL2 的空名称空条件
            // 搜索就是这个列表）。这里以前会拿 rootId/versionId 反推游戏版本 +
            // 加载器去收窄分面，结果是：版本目录叫"整合包名"时被当成
            // `versions:<名字>` 去查，整页 0 命中，界面还甩一句"这个筛选条件下
            // 没有模组"。筛选条件只接受显式传入，绝不由当前选中版本推出。
            to_value(
                mods(kernel)?
                    .ranking(ModRankingQuery {
                        game_version: a.game_version,
                        loader: a.loader,
                        type_id: a.type_id,
                        page: a.page,
                        force: a.force,
                    })
                    .map_err(|e| e.to_string())?,
            )
        }
        "mods_search" => {
            let a: ModSearchArgs = parse(args)?;
            to_value(
                mods(kernel)?
                    .search(ModSearchQuery {
                        query: a.query,
                        source: a.source,
                        type_id: a.type_id,
                        page_size: a.page_size,
                        page: a.page,
                        force: a.force,
                        latest_game_version: a.latest_game_version,
                    })
                    .map_err(|e| e.to_string())?,
            )
        }
        "mods_detail" => {
            let a: ModDetailArgs = parse(args)?;
            to_value(
                mods(kernel)?
                    .detail(ModDetailQuery {
                        source: a.source,
                        project_id: a.project_id,
                        root_id: a.root_id,
                        version_id: a.version_id,
                        force: a.force,
                    })
                    .map_err(|e| e.to_string())?,
            )
        }
        "mods_install" => {
            let a: ModInstallRequest = parse(args)?;
            to_value(mods(kernel)?.install(&a).map_err(|e| e.to_string())?)
        }

        // ---- 本地内容（模组 / 资源包 / 存档 / 日志） ----
        "content_dirs" => {
            let a: ContentArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .dirs(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "content_mods_list" => {
            let a: ContentArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .mods(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "content_mods_set_enabled" => {
            let a: ContentToggleArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .set_mod_enabled(&a.root_id, &a.version_id, &a.file, a.enabled)
                    .map_err(|e| e.to_string())?,
            )
        }
        // 图标按需取：列表里只给 `hasIcon`，滚动到哪一行才请求哪一行的图。
        "content_mod_icon" => {
            let a: ModIconArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .mod_icon(&a.root_id, &a.version_id, &a.file_name)
                    .map_err(|e| e.to_string())?,
            )
        }
        "mods_updates_check" => {
            let a: VersionArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .check_updates(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "mods_apply_update" => {
            let a: ModApplyUpdateArgs = parse(args)?;
            content(kernel)?
                .apply_update(&a.root_id, &a.version_id, &a.update)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "content_packs_list" => {
            let a: ContentArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .resource_packs(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "content_saves_list" => {
            let a: ContentArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .saves(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "content_logs_list" => {
            let a: ContentArgs = parse(args)?;
            to_value(
                content(kernel)?
                    .logs(&a.root_id, &a.version_id)
                    .map_err(|e| e.to_string())?,
            )
        }
        "content_logs_read" => {
            let a: ContentLogArgs = parse(args)?;
            let max = a.max_bytes.unwrap_or(LOG_TAIL_DEFAULT as u64) as usize;
            to_value(
                content(kernel)?
                    .read_log(&a.root_id, &a.version_id, &a.file, max)
                    .map_err(|e| e.to_string())?,
            )
        }
        "content_import" => {
            let a: ContentImportArgs = parse(args)?;
            let paths: Vec<std::path::PathBuf> =
                a.paths.iter().map(std::path::PathBuf::from).collect();
            to_value(
                content(kernel)?
                    .import(&a.root_id, &a.version_id, a.kind, &paths)
                    .map_err(|e| e.to_string())?,
            )
        }
        "content_delete" => {
            let a: ContentFileArgs = parse(args)?;
            content(kernel)?
                .delete(&a.root_id, &a.version_id, a.kind, &a.file)
                .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }

        // ---- Java 运行时 ----
        "java_list" => {
            let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
            to_value(java(kernel)?.list(force).map_err(|e| e.to_string())?)
        }
        "java_inspect" => {
            let a: JavaInspectArgs = parse(args)?;
            to_value(
                java(kernel)?
                    .inspect(Path::new(&a.path))
                    .map_err(|e| e.to_string())?,
            )
        }
        "java_pick" => {
            let a: JavaPickArgs = parse(args)?;
            to_value(
                java(kernel)?
                    .pick(a.major, a.java_path.as_deref())
                    .map_err(|e| e.to_string())?,
            )
        }
        "versions_verify" => {
            let a: ContentArgs = parse(args)?;
            let root = roots(kernel)?.get(&a.root_id).map_err(|e| e.to_string())?;
            let integrity = versions(kernel)?
                .verify(Path::new(&root.path), &a.version_id)
                .map_err(|e| e.to_string())?;
            to_value(integrity.report(&a.version_id))
        }

        // ---- 诊断 ----
        "sources_list" => {
            let list: Vec<Value> = all_install_sources(ctx)
                .into_iter()
                .map(|s| {
                    json!({
                        "id": s.id(),
                        "label": s.label(),
                        "priority": s.priority(),
                        "manifestUrl": s.manifest_url(),
                        "baseUrl": s.base_url(),
                    })
                })
                .collect();
            Ok(Value::Array(list))
        }

        other => Err(format!("未知命令 `{other}`")),
    }
}

/// 界面启动时需要的"一次性快照"，避免开屏发十几条命令。
pub fn bootstrap_snapshot(kernel: &Kernel) -> Value {
    let ctx = kernel.context();
    let roots_service = ctx.service::<RootRegistry>();
    let roots_list = roots_service
        .as_ref()
        .and_then(|r| r.list().ok())
        .unwrap_or_default();
    let selected = roots_service
        .as_ref()
        .and_then(|r| r.selected().ok().flatten());
    let global = ctx.service::<Settings>().and_then(|s| s.global().ok());
    let accounts_list = ctx
        .service::<Accounts>()
        .and_then(|a| a.list().ok())
        .unwrap_or_default();
    let active = ctx
        .service::<Accounts>()
        .and_then(|a| a.active().ok().flatten());

    json!({
        "roots": roots_list,
        "selectedRootId": selected.map(|r| r.id),
        "globalSettings": global,
        "accounts": accounts_list,
        "activeAccount": active,
        "plugins": describe_plugins(kernel),
        "sources": all_install_sources(ctx)
            .into_iter()
            .map(|s| json!({"id": s.id(), "label": s.label(), "priority": s.priority()}))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logger_records(kernel: &Kernel) -> (String, Vec<Value>) {
        let logger = kernel.context().service::<kaleid_core_plugins::LauncherLogger>().unwrap();
        assert_eq!(logger.flush(), kaleid_core_plugins::LoggerControlOutcome::Flushed);
        let mut text = String::new();
        for entry in std::fs::read_dir(logger.run_dir()).unwrap().flatten() {
            if entry.file_name().to_string_lossy().starts_with("launcher.log") {
                text.push_str(&std::fs::read_to_string(entry.path()).unwrap());
            }
        }
        let records = text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
        (text, records)
    }

    // Scan every persisted launcher run and archive, not only the current active log.
    fn logger_all_launcher_records(kernel: &Kernel, data_root: &Path) -> (String, Vec<Value>) {
        let logger = kernel.context().service::<kaleid_core_plugins::LauncherLogger>().unwrap();
        assert_eq!(logger.flush(), kaleid_core_plugins::LoggerControlOutcome::Flushed);
        let mut text = String::new();
        for run in std::fs::read_dir(data_root.join("logs/launcher")).unwrap() {
            let run = run.unwrap();
            if !run.file_type().unwrap().is_dir() { continue; }
            for entry in std::fs::read_dir(run.path()).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_file() && entry.file_name().to_string_lossy().starts_with("launcher.log") {
                    text.push_str(&std::fs::read_to_string(entry.path()).unwrap());
                }
            }
        }
        let records = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        (text, records)
    }

    fn root_provenance_update_args(root_id: &str, file_name: &str) -> Value {
        json!({
            "rootId": root_id, "versionId": "1.21.4",
            "update": {
                "fileName": file_name, "modId": "fixture", "source": "modrinth",
                "projectId": "AANobbMI", "currentVersion": "1", "latestVersion": "2",
                "latestFileName": "fixture-new.jar", "downloadUrl": "https://cdn.modrinth.com/fixture.jar",
                "size": 1, "sha1": null, "pageUrl": null
            }
        })
    }

    fn assert_unknown_root_domain_logs_are_private(minimal: bool) {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, minimal).unwrap();
        let sentinel = "SENTINEL_TOKEN_FINAL_REVIEW";
        let install = host.dispatch("mods_install", json!({
            "rootId": sentinel, "versionId": "1.21.4", "source": "modrinth",
            "projectId": "AANobbMI", "fileId": "AANobbMJ", "overwrite": false
        })).unwrap_err();
        let update = host.dispatch("mods_apply_update", root_provenance_update_args(sentinel, "fixture-old.jar")).unwrap_err();
        let original_error = if minimal {
            "插件未就绪：下载服务不可用，请先启用「版本管理」插件".to_string()
        } else {
            format!("未找到：文件夹 {sentinel}")
        };
        assert_eq!(install, original_error);
        assert_eq!(update, original_error, "keep downloader-before-root business validation order");
        let (text, records) = logger_all_launcher_records(host.kernel(), &data);
        assert!(!text.contains(sentinel), "unestablished root leaked through nested domain diagnostics");
        for (component, operation) in [("mods", "mod_install"), ("content", "mod_update")] {
            let domain: Vec<_> = records.iter().filter(|r| r["component"] == component && r["fields"]["operation"] == operation).collect();
            let ends: Vec<_> = domain.iter().filter(|r| r["event"] == "operation.finished").collect();
            assert_eq!(ends.len(), 1);
            assert_eq!(ends[0]["fields"]["outcome"], "failed");
            assert_eq!(ends[0]["fields"]["category"], if minimal { "unavailable" } else { "not_found" });
            assert!(domain.iter().all(|r| r["fields"].get("root_id").is_none()));
            assert_eq!(domain.iter().filter(|r| r["event"] == "operation.accepted").count(), 1);
        }
    }

    #[test]
    fn logger_domain_unknown_root_standard_never_persists_unestablished_identity() {
        assert_unknown_root_domain_logs_are_private(false);
    }

    #[test]
    fn logger_domain_unknown_root_minimal_never_persists_unestablished_identity() {
        assert_unknown_root_domain_logs_are_private(true);
    }

    #[test]
    fn logger_domain_validated_root_is_retained_only_after_existing_resolution() {
        for minimal in [false, true] {
            let (_dir, data, mc) = env();
            let host = Host::boot(&data, minimal).unwrap();
            let root = host.dispatch("roots_add", json!({"path": mc.to_string_lossy()})).unwrap();
            let root_id = root["id"].as_str().unwrap();
            let error = host.dispatch("mods_apply_update", root_provenance_update_args(root_id, "../invalid.jar")).unwrap_err();
            assert_eq!(error, if minimal {
                "插件未就绪：下载服务不可用，请先启用「版本管理」插件"
            } else {
                "非法输入：非法的文件名：../invalid.jar"
            });
            let (_, records) = logger_all_launcher_records(host.kernel(), &data);
            let domain: Vec<_> = records.iter().filter(|r| r["component"] == "content" && r["fields"]["operation"] == "mod_update").collect();
            let accepted = domain.iter().find(|r| r["event"] == "operation.accepted").unwrap();
            assert!(accepted["fields"].get("root_id").is_none(), "acceptance precedes existing area_dir validation");
            let ended: Vec<_> = domain.iter().filter(|r| r["event"] == "operation.finished").collect();
            assert_eq!(ended.len(), 1);
            if minimal {
                assert!(ended[0]["fields"].get("root_id").is_none(), "unavailable downloader prevents root establishment");
            } else {
                assert_eq!(ended[0]["fields"]["root_id"], root_id, "retain genuine root on the domain result, not only the Host command");
            }
        }
    }

    fn command_results<'a>(records: &'a [Value], command: &str) -> Vec<&'a Value> {
        records.iter().filter(|r| r["event"] == "command.result" && r["fields"]["command"] == command).collect()
    }

    #[test]
    fn logger_unknown_plugin_targets_never_persist_as_kernel_identities() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let sentinel = "SENTINEL_TOKEN_3A";
        for enabled in [false, true] {
            let error = host.dispatch("plugins_set_enabled", json!({"id": sentinel, "enabled": enabled})).unwrap_err();
            assert!(error.contains(sentinel), "business error must remain unchanged");
        }
        host.dispatch("plugins_set_enabled", json!({"id": "news", "enabled": false})).unwrap();
        host.dispatch("plugins_set_enabled", json!({"id": "news", "enabled": true})).unwrap();
        let (text, records) = logger_records(host.kernel());
        assert!(!text.contains(sentinel), "unregistered target leaked through kernel lifecycle");
        for event in ["plugin.disable", "plugin.enable"] {
            let rejected: Vec<_> = records.iter().filter(|r| r["component"] == "kernel" && r["event"] == event && r["fields"]["category"] == "not_loaded").collect();
            assert_eq!(rejected.len(), 1);
            assert_eq!(rejected[0]["fields"]["outcome"], "rejected");
            assert!(rejected[0]["fields"].get("plugin_id").is_none());
            assert!(records.iter().any(|r| r["component"] == "kernel" && r["event"] == event && r["fields"]["plugin_id"] == "news" && r["fields"]["outcome"] == "succeeded"));
        }
    }

    #[test]
    fn logger_lazy_free_state_survives_ambient_plugin_disposal() {
        let (_dir, data, _mc) = env();
        let kernel = Kernel::new();
        let _ = kaleid_core_plugins::bootstrap_logger(&kernel, &data);
        kernel.context().with_owner("news", |_| {
            assert!(dispatch(&kernel, "downloads_snapshot", json!({})).is_err());
        });
        kernel.context().dispose_owner("news");
        assert!(dispatch(&kernel, "downloads_snapshot", json!({})).is_err());
        let (_, records) = logger_records(&kernel);
        assert_eq!(command_results(&records, "downloads_snapshot").len(), 1, "plugin disposal reset the failure episode");
        let state = command_diagnostics(&kernel);
        state.shutdown_started.store(true, Ordering::Release);
        kernel.context().dispose_owner("news");
        let retained = command_diagnostics(&kernel);
        assert!(Arc::ptr_eq(&state, &retained));
        assert!(retained.shutdown_started.load(Ordering::Acquire));
    }

    #[test]
    fn logger_boot_establishes_root_state_before_public_commands() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, true).unwrap();
        assert!(host.kernel().context().service::<CommandDiagnostics>().is_some());
        assert!(host.kernel().context().registry_snapshot().iter().any(|(service, owner, _)| {
            *service == "host-command-diagnostics" && owner.is_empty()
        }));
    }

    #[test]
    fn logger_first_free_poll_during_reapply_survives_plugin_disable() {
        struct Reapply {
            attempts: std::sync::atomic::AtomicUsize,
            entered: Arc<std::sync::Barrier>,
            release: Arc<std::sync::Barrier>,
        }
        impl kaleid_kernel::Plugin for Reapply {
            fn id(&self) -> &str { "news" }
            fn apply(&self, _ctx: &kaleid_kernel::Context, _cfg: Value) -> kaleid_kernel::PluginResult {
                if self.attempts.fetch_add(1, Ordering::SeqCst) != 0 {
                    self.entered.wait();
                    self.release.wait();
                }
                Ok(())
            }
        }
        let (_dir, data, _mc) = env();
        let kernel = Arc::new(Kernel::new());
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let plugin = Arc::new(Reapply {
            attempts: std::sync::atomic::AtomicUsize::new(0),
            entered: entered.clone(),
            release: release.clone(),
        });
        kernel.register_factory("news", move || plugin.clone() as Arc<dyn kaleid_kernel::Plugin>);
        let _ = kaleid_core_plugins::bootstrap_logger(&kernel, &data);
        kernel.load(&[kaleid_kernel::PluginSpec::new("news")]).unwrap();
        kernel.disable("news").unwrap();
        let applying = kernel.clone();
        let worker = std::thread::spawn(move || applying.enable("news"));
        entered.wait();
        let first = dispatch(&kernel, "downloads_snapshot", json!({}));
        release.wait();
        worker.join().unwrap().unwrap();
        assert!(first.is_err());
        kernel.disable("news").unwrap();
        assert!(dispatch(&kernel, "downloads_snapshot", json!({})).is_err());
        let (_, records) = logger_records(&kernel);
        assert_eq!(command_results(&records, "downloads_snapshot").len(), 1);
    }

    // Missing bootstrap or wrapping Host and free dispatch twice must break this test.
    #[test]
    fn logger_startup_and_each_public_gateway_persist_once() {
        for minimal in [false, true] {
            let (_dir, data, _mc) = env();
            let host = Host::boot(&data, minimal).unwrap();
            host.dispatch("roots_list", json!({})).unwrap();
            dispatch(host.kernel(), "sources_list", json!({})).unwrap();
            host.dispatch("plugins_slots", json!({})).unwrap();
            host.dispatch("plugins_manifests", json!({})).unwrap();
            let (_, records) = logger_records(host.kernel());
            assert_eq!(records.iter().filter(|r| r["event"] == "startup.result" && r["fields"]["outcome"] == "succeeded").count(), 1);
            for (command, component) in [("roots_list", "core-lists"), ("sources_list", "version-manager"), ("plugins_slots", "manifests"), ("plugins_manifests", "manifests")] {
                let results = command_results(&records, command);
                assert_eq!(results.len(), 1, "{command}: {records:?}");
                assert_eq!(results[0]["component"], component);
                assert_eq!(results[0]["fields"]["outcome"], "succeeded");
                assert!(results[0]["fields"]["duration_ms"].is_u64());
                let starts: Vec<_> = records.iter().filter(|r| r["event"] == "command.start" && r["fields"]["command"] == command).collect();
                assert_eq!(starts.len(), 1);
                assert_eq!(starts[0]["operation_id"], results[0]["operation_id"]);
            }
        }
    }

    // Keep diagnostic production outside the match: ?, unknown and Ok(blocked) are observable.
    #[test]
    fn logger_errors_early_blocks_and_payloads_are_safe() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let root = host.dispatch("roots_add", json!({"path": mc.to_string_lossy()})).unwrap();
        let root_id = root["id"].as_str().unwrap();
        let blocked = host.dispatch("launch_check", json!({"rootId": root_id, "versionId": "1.21.4"})).unwrap();
        assert_eq!(blocked["ok"], false);
        let sentinels = ["SENTINEL_TOKEN_3A", "DEVICECODE_3A", "-DprivateJvm=3A", "https://user:credential3A@example.invalid/private", "SecretUser_42"];
        assert!(host.dispatch("settings_patch_global", json!({"patch": {"jvmArgs": sentinels}})).is_ok());
        host.dispatch("roots_add", json!({"path": sentinels[3], "alias": sentinels[4]})).unwrap();
        assert!(dispatch(host.kernel(), sentinels[0], json!({"rootId": sentinels[1], "versionId": sentinels[4], "sessionId": sentinels[2]})).is_err());
        assert!(host.dispatch("versions_scan", json!({"rootId": sentinels[1]})).is_err());
        assert!(host.dispatch("accounts_apply_offline", json!({"name": sentinels[4]})).is_ok());
        assert!(host.dispatch("roots_remove", json!({"id": [sentinels[0], sentinels[1]]})).is_err());
        assert!(host.dispatch("plugins_set_enabled", json!({"id": "launcher-logger", "enabled": false})).is_err());
        let (text, records) = logger_records(host.kernel());
        for sentinel in sentinels { assert!(!text.contains(sentinel), "leaked {sentinel}"); }
        assert_eq!(command_results(&records, "unknown").len(), 1);
        assert_eq!(command_results(&records, "unknown")[0]["fields"]["outcome"], "failed");
        assert_eq!(command_results(&records, "versions_scan")[0]["fields"]["category"], "command_failed");
        assert_eq!(command_results(&records, "roots_remove")[0]["fields"]["outcome"], "failed");
        let checks = command_results(&records, "launch_check");
        assert_eq!(checks[0]["fields"]["outcome"], "blocked");
        assert_eq!(checks[0]["fields"]["category"], "no-account");
        assert_eq!(checks[0]["fields"]["root_id"], root_id);
        assert_eq!(command_results(&records, "plugins_set_enabled")[0]["fields"]["outcome"], "failed");
    }

    // Successful polls stay silent; one failure episode is local to this kernel and resets.
    #[test]
    fn logger_poll_failures_coalesce_until_success_per_kernel() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        host.kernel().disable("version-manager").unwrap();
        for _ in 0..3 {
            host.dispatch("launch_states", json!({})).unwrap();
            host.dispatch("launch_state", json!({})).unwrap();
            assert!(host.dispatch("downloads_snapshot", json!({})).is_err());
            assert!(dispatch(host.kernel(), "msa_poll", json!({})).is_err());
        }
        host.kernel().enable("version-manager").unwrap();
        host.dispatch("downloads_snapshot", json!({})).unwrap();
        host.kernel().disable("version-manager").unwrap();
        for _ in 0..2 { assert!(dispatch(host.kernel(), "downloads_snapshot", json!({})).is_err()); }
        let (_, records) = logger_records(host.kernel());
        assert!(command_results(&records, "launch_states").is_empty());
        assert!(command_results(&records, "launch_state").is_empty());
        assert_eq!(command_results(&records, "downloads_snapshot").len(), 2);
        assert_eq!(command_results(&records, "msa_poll").len(), 1);
        assert!(!records.iter().any(|r| r["event"] == "command.start" && r["fields"]["command"] == "downloads_snapshot"));
        let (_other_dir, other_data, _mc) = env();
        let other = Host::boot(&other_data, true).unwrap();
        assert!(other.dispatch("downloads_snapshot", json!({})).is_err());
        assert_eq!(command_results(&logger_records(other.kernel()).1, "downloads_snapshot").len(), 1);
    }

    // Exercise the real producer boundary without starting detached download/game workers.
    #[test]
    fn logger_queue_results_are_accepted_not_terminal_success() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let job = "d486b9fc-8459-4c25-a5ab-348e3b63fb84";
        for command in ["versions_install", "versions_update_loader", "mods_install", "launch_start"] {
            let response = if command == "launch_start" { json!({"sessionId": 7, "phase":"preparing", "message":"PRIVATE_RESPONSE_3A"}) } else { json!({"jobId": job, "message":"PRIVATE_RESPONSE_3A"}) };
            let result = observe_command(host.kernel(), command, json!({}), |_| Ok(response.clone())).unwrap();
            assert_eq!(result, response);
        }
        observe_command(host.kernel(), "mods_apply_update", json!({}), |_| Ok(Value::Null)).unwrap();
        let (text, records) = logger_records(host.kernel());
        assert!(!text.contains("PRIVATE_RESPONSE_3A"));
        for command in ["versions_install", "versions_update_loader", "mods_install", "launch_start"] {
            let results = command_results(&records, command);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0]["fields"]["outcome"], "accepted");
            if command == "launch_start" { assert_eq!(results[0]["session_id"], "7"); }
            else { assert_eq!(results[0]["job_id"], job); }
        }
        assert_eq!(command_results(&records, "mods_apply_update")[0]["fields"]["outcome"], "succeeded");
    }

    #[test]
    fn logger_shutdown_is_once_and_keeps_business_services_alive() {
        let (_dir, data, _mc) = env();
        let host = Arc::new(Host::boot(&data, false).unwrap());
        let logger = host.kernel().context().service::<kaleid_core_plugins::LauncherLogger>().unwrap();
        let run_dir = logger.run_dir().to_path_buf();
        let left = host.clone();
        let right = host.clone();
        let a = std::thread::spawn(move || left.shutdown_logging());
        let b = std::thread::spawn(move || right.shutdown_logging());
        a.join().unwrap();
        b.join().unwrap();
        host.shutdown_logging();
        assert!(host.kernel().context().service::<kaleid_core_plugins::LauncherLogger>().is_none());
        assert!(host.dispatch("roots_list", json!({})).unwrap().is_array());
        drop(host);
        let text = std::fs::read_to_string(run_dir.join("launcher.log")).unwrap();
        let records: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(records.iter().filter(|r| r["event"] == "launcher.shutdown").count(), 1);
        assert_eq!(records.iter().filter(|r| r["event"] == "writer.shutdown").count(), 1);
    }

    #[test]
    fn logger_boot_failure_is_persisted_without_business_error_text() {
        let (_dir, data, _mc) = env();
        let plugin = data.join("plugins/bad-config");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("manifest.json"), json!({
            "id":"bad-config", "name":"bad", "version":"1", "api":"1",
            "steps":[{"op":"update-config", "target":"version-manager", "patch":{"enableMojang":"PRIVATE_BOOT_ERROR_3A"}}]
        }).to_string()).unwrap();
        assert!(Host::boot(&data, false).is_err());
        let mut text = String::new();
        for run in std::fs::read_dir(data.join("logs/launcher")).unwrap().flatten() {
            text.push_str(&std::fs::read_to_string(run.path().join("launcher.log")).unwrap());
        }
        let records: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert!(records.iter().any(|r| r["event"] == "startup.result" && r["fields"]["outcome"] == "failed"));
        assert_eq!(records.iter().filter(|r| r["event"] == "writer.shutdown").count(), 1);
        assert!(!text.contains("PRIVATE_BOOT_ERROR_3A"));
    }

    #[test]
    fn logger_degraded_output_does_not_change_dispatch_results() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "x").unwrap();
        let kernel = Kernel::new();
        let logger = kaleid_core_plugins::bootstrap_logger(&kernel, &blocked);
        assert!(dispatch(&kernel, "jvm_default_args", json!({})).unwrap().is_array());
        let error = dispatch(&kernel, "PRIVATE_UNKNOWN_3A", json!({})).unwrap_err();
        assert!(error.contains("PRIVATE_UNKNOWN_3A"));
        assert_eq!(logger.flush(), kaleid_core_plugins::LoggerControlOutcome::Degraded);
    }

    // Bootstrap must already observe validation/apply failures before Kernel::load.
    #[test]
    fn logger_manifest_rejections_are_safe_and_cannot_disable_sink() {
        let (_dir, data, _mc) = env();
        let invalid = data.join("plugins/PRIVATE_DIRECTORY_3A");
        let valid = data.join("plugins/logger-boundary");
        std::fs::create_dir_all(&invalid).unwrap();
        std::fs::create_dir_all(&valid).unwrap();
        std::fs::write(invalid.join("manifest.json"), "PRIVATE_MANIFEST_ERROR_3A").unwrap();
        std::fs::write(valid.join("manifest.json"), json!({
            "id":"logger-boundary", "name":"boundary", "version":"1", "api":"1",
            "steps":[{"op":"disable-plugin", "target":"launcher-logger"},
                     {"op":"set-default-setting", "key":"memoryMb", "value":"PRIVATE_DEFAULT_3A"}]
        }).to_string()).unwrap();
        let host = Host::boot(&data, false).unwrap();
        host.dispatch("roots_list", json!({})).unwrap();
        let (text, records) = logger_records(host.kernel());
        for sentinel in ["PRIVATE_DIRECTORY_3A", "PRIVATE_MANIFEST_ERROR_3A", "PRIVATE_DEFAULT_3A"] { assert!(!text.contains(sentinel)); }
        for event in ["manifest.validation", "manifest.apply", "manifest.default"] {
            assert!(records.iter().any(|r| r["event"] == event && r["fields"]["outcome"] == "rejected"), "missing {event}: {records:?}");
        }
        assert_eq!(command_results(&records, "roots_list").len(), 1);
    }

    fn env() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let mc = dir.path().join(".minecraft");
        std::fs::create_dir_all(mc.join("versions/1.21.4")).unwrap();
        std::fs::write(
            mc.join("versions/1.21.4/1.21.4.json"),
            serde_json::to_string(&json!({
                "id": "1.21.4",
                "mainClass": "net.minecraft.client.main.Main",
                "arguments": { "game": ["--username", "${auth_player_name}"] }
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(mc.join("versions/1.21.4/1.21.4.jar"), b"jar").unwrap();
        (dir, data, mc)
    }

    /// 「默认 JVM 参数」这件事的两端：
    /// 界面的「恢复默认」按钮从 `jvm_default_args` 拿清单，
    /// 而全新数据目录里 `settings_global` 那一栏本来就该是同一份内容。
    #[test]
    fn jvm_defaults_are_exposed_and_prefilled() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();

        let defaults = host.dispatch("jvm_default_args", json!({})).unwrap();
        let list: Vec<String> = serde_json::from_value(defaults).unwrap();
        assert_eq!(list.len(), 4);
        assert!(list.iter().any(|a| a == "-XX:-OmitStackTraceInFastThrow"));

        // 全新目录：设置里这一栏就是那四条（用户打开面板能直接看到、能改能删）
        let global = host.dispatch("settings_global", json!({})).unwrap();
        let stored: Vec<String> = serde_json::from_value(global["jvmArgs"].clone()).unwrap();
        assert_eq!(stored, list);
        assert_eq!(global["jvmArgsInitialized"], true);

        // 用户清空 → 保存的就是空，读回来还是空（不会被"默认"顶回来）
        host.dispatch("settings_patch_global", json!({ "patch": { "jvmArgs": [] } }))
            .unwrap();
        let global = host.dispatch("settings_global", json!({})).unwrap();
        assert_eq!(global["jvmArgs"], json!([]));

        // 界面靠这条判断哪些开关有意义：「使用高性能显卡」只在 Windows 生效
        let platform = host.dispatch("app_platform", json!({})).unwrap();
        assert_eq!(platform["os"], std::env::consts::OS);
        assert_eq!(
            platform["highPerformanceGpuSupported"].as_bool().unwrap(),
            cfg!(windows)
        );
    }

    #[test]
    fn command_table_covers_the_bridge_contract() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();

        // roots_add / list
        let added = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = added["id"].as_str().unwrap().to_string();
        let list = host.dispatch("roots_list", json!({})).unwrap();
        assert!(list
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == json!(root_id)));

        // roots_rename / set_isolation
        let renamed = host
            .dispatch("roots_rename", json!({ "id": root_id, "alias": "主力" }))
            .unwrap();
        assert_eq!(renamed["alias"], json!("主力"));
        let iso = host
            .dispatch(
                "roots_set_isolation",
                json!({ "id": root_id, "mode": "off" }),
            )
            .unwrap();
        assert_eq!(iso["isolation"], json!("off"));

        // versions_scan
        let versions = host
            .dispatch("versions_scan", json!({ "rootId": root_id }))
            .unwrap();
        assert_eq!(versions.as_array().unwrap().len(), 1);
        assert_eq!(versions[0]["id"], json!("1.21.4"));

        // settings
        let global = host
            .dispatch(
                "settings_patch_global",
                json!({ "patch": { "memoryMb": 6144 } }),
            )
            .unwrap();
        assert_eq!(global["memoryMb"], json!(6144));
        let effective = host
            .dispatch(
                "settings_effective",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(effective["memoryMb"], json!(6144));
        // 刚才把文件夹隔离设成了 off
        assert_eq!(effective["isolated"], json!(false));

        // 账号：输入即生效
        let acc = host
            .dispatch("accounts_apply_offline", json!({ "name": "Player_123" }))
            .unwrap();
        assert_eq!(acc["name"], json!("Player_123"));
        let bad = host.dispatch("accounts_apply_offline", json!({ "name": "坏!" }));
        assert!(bad.is_err());
        let active = host.dispatch("accounts_active", json!({})).unwrap();
        assert_eq!(active["name"], json!("Player_123"));

        // 启动计划
        let plan = host
            .dispatch(
                "launch_plan",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(plan["username"], json!("Player_123"));
        assert!(plan["classpath"].as_array().unwrap().len() >= 1);

        // 插件开关：停用 version-manager 后下载源消失，但列表仍在
        assert!(!host
            .dispatch("sources_list", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        let after = host
            .dispatch(
                "plugins_set_enabled",
                json!({ "id": "version-manager", "enabled": false }),
            )
            .unwrap();
        let vm = after
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == json!("version-manager"))
            .unwrap();
        assert_eq!(vm["enabled"], json!(false));
        assert!(host
            .dispatch("sources_list", json!({}))
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        // 版本列表仍在
        assert_eq!(
            host.dispatch("versions_scan", json!({ "rootId": root_id }))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn version_delete_and_loader_status() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let added = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = added["id"].as_str().unwrap().to_string();

        // 纯原版：没有加载器可以更新。
        let status = host
            .dispatch(
                "versions_loader_status",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(status["loader"], json!("vanilla"));
        assert_eq!(status["current"], json!(null));
        assert_eq!(status["newerAvailable"], json!(false));

        // 给这个版本补一个加载器标记，状态里就应该能读到 Fabric。
        let dir = mc.join("versions/1.21.4");
        std::fs::create_dir_all(dir.join(".fabric")).unwrap();
        std::fs::write(
            dir.join(".fabric/loader.json"),
            json!({"kind": "fabric", "loader": "0.16.9", "game": "1.21.4"}).to_string(),
        )
        .unwrap();
        let status = host
            .dispatch(
                "versions_loader_status",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(status["loader"], json!("fabric"));
        assert_eq!(status["current"], json!("0.16.9"));

        // 删除：目录与设置一起消失，并且报告释放的字节数。
        let deleted = host
            .dispatch(
                "versions_delete",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(deleted["versionId"], json!("1.21.4"));
        assert!(deleted["freedBytes"].as_u64().unwrap() > 0);
        assert!(!dir.exists());
        assert!(host
            .dispatch(
                "versions_scan",
                json!({ "rootId": root_id })
            )
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn bootstrap_snapshot_shape() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        host.dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let snap = bootstrap_snapshot(host.kernel());
        for key in [
            "roots",
            "selectedRootId",
            "globalSettings",
            "accounts",
            "activeAccount",
            "plugins",
            "sources",
        ] {
            assert!(snap.get(key).is_some(), "快照缺少 {key}");
        }
    }

    #[test]
    fn unknown_command_lists_nothing_dangerous() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let err = host.dispatch("rm_rf", json!({})).unwrap_err();
        assert!(err.contains("未知命令"));
    }

    #[test]
    fn downloads_snapshot_is_real_and_follows_concurrency_setting() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let initial = host.dispatch("downloads_snapshot", json!({})).unwrap();
        assert_eq!(initial["jobs"], json!([]));
        assert_eq!(initial["concurrency"], json!(8));

        host.dispatch(
            "settings_patch_global",
            json!({ "patch": { "downloadConcurrency": 256 } }),
        )
        .unwrap();
        let after = host.dispatch("downloads_snapshot", json!({})).unwrap();
        assert_eq!(after["concurrency"], json!(256));
        assert!(host
            .dispatch(
                "settings_patch_global",
                json!({ "patch": { "downloadConcurrency": 257 } }),
            )
            .is_err());
    }

    #[test]
    fn version_name_check_rejects_invalid_and_duplicate_names() {
        let (_dir, data, mc) = env();
        std::fs::create_dir_all(mc.join("versions/1.21.4")).unwrap();
        std::fs::write(
            mc.join("versions/1.21.4/1.21.4.json"),
            r#"{"id":"1.21.4","mainClass":"a.B"}"#,
        )
        .unwrap();
        let host = Host::boot(&data, false).unwrap();
        let root = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = root["id"].as_str().unwrap();

        let bad = host
            .dispatch(
                "versions_check_name",
                json!({ "rootId": root_id, "name": "bad." }),
            )
            .unwrap();
        assert_eq!(bad["ok"], json!(false));
        assert!(bad["error"].as_str().unwrap().contains("小数点"));

        let duplicate = host
            .dispatch(
                "versions_check_name",
                json!({ "rootId": root_id, "name": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(duplicate["ok"], json!(false));
        assert!(duplicate["error"].as_str().unwrap().contains("重名"));

        let good = host
            .dispatch(
                "versions_check_name",
                json!({ "rootId": root_id, "name": "1.20.1-Fabric 0.19.2" }),
            )
            .unwrap();
        assert_eq!(good["ok"], json!(true));
    }

    #[test]
    fn manifests_add_sources_slots_and_defaults() {
        let (_dir, data, _mc) = env();
        let plugin_dir = data.join("plugins/example-mirror");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.json"),
            serde_json::to_string(&json!({
                "id": "example-mirror",
                "name": "校园镜像源",
                "version": "1.0.0",
                "api": "1",
                "steps": [
                    {
                        "op": "provide-source",
                        "id": "campus-mirror",
                        "label": "校园网镜像",
                        "priority": 20,
                        "manifestUrl": "https://mirror.example.edu/minecraft/version_manifest_v2.json",
                        "baseUrl": "https://mirror.example.edu/minecraft"
                    },
                    {
                        "op": "bind-slot",
                        "slot": "overview.notices",
                        "title": "镜像说明",
                        "view": "readonly-text:校园网内优先走本地镜像。"
                    },
                    { "op": "set-default-setting", "key": "checkBeforeLaunch", "value": true }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let host = Host::boot(&data, false).unwrap();

        // 下载源：清单挂的那条在，内置两条也还在。
        let sources = host.dispatch("sources_list", json!({})).unwrap();
        let ids: Vec<String> = sources
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect();
        assert!(ids.contains(&"campus-mirror".to_string()), "{ids:?}");
        assert!(ids.contains(&"mojang".to_string()), "{ids:?}");
        assert!(ids.contains(&"bmclapi".to_string()), "{ids:?}");

        // 面板：只读文本，插槽名是追加型的 overview.notices。
        let slots = host.dispatch("plugins_slots", json!({})).unwrap();
        let first = &slots.as_array().unwrap()[0];
        assert_eq!(first["slot"], json!("overview.notices"));
        assert_eq!(first["text"], json!("校园网内优先走本地镜像。"));

        // 安装状态可见。
        let manifests = host.dispatch("plugins_manifests", json!({})).unwrap();
        assert_eq!(manifests.as_array().unwrap()[0]["state"], json!("ready"));
    }

    #[test]
    fn manifest_default_does_not_override_an_existing_setting() {
        let (_dir, data, _mc) = env();
        std::fs::create_dir_all(&data).unwrap();
        // 用户已经显式把 checkBeforeLaunch 设成 false。
        std::fs::write(
            data.join("settings.json"),
            serde_json::to_string(&json!({ "checkBeforeLaunch": false })).unwrap(),
        )
        .unwrap();
        let plugin_dir = data.join("plugins/example-settings");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.json"),
            serde_json::to_string(&json!({
                "id": "example-settings",
                "name": "默认启动行为",
                "version": "1.0.0",
                "api": "1",
                "steps": [
                    { "op": "set-default-setting", "key": "checkBeforeLaunch", "value": true }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let host = Host::boot(&data, false).unwrap();
        let global = host.dispatch("settings_global", json!({})).unwrap();
        assert_eq!(
            global["checkBeforeLaunch"],
            json!(false),
            "用户显式设置的值不能被清单默认值覆盖"
        );
    }

    #[test]
    fn news_plugin_is_built_in_and_disableable() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let plugins = host.dispatch("plugins_list", json!({})).unwrap();
        let news = plugins
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == json!("news"))
            .expect("news plugin should be registered");
        assert_eq!(news["enabled"], json!(true));

        let after = host
            .dispatch(
                "plugins_set_enabled",
                json!({ "id": "news", "enabled": false }),
            )
            .unwrap();
        let news = after
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == json!("news"))
            .unwrap();
        assert_eq!(news["enabled"], json!(false));
    }

    #[test]
    fn launch_plan_without_account_is_a_clear_error() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let root = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let id = root["id"].as_str().unwrap();
        let err = host
            .dispatch(
                "launch_plan",
                json!({ "rootId": id, "versionId": "1.21.4" }),
            )
            .unwrap_err();
        assert!(err.contains("还没有账号"), "got {err}");
    }

    #[test]
    fn mods_commands_are_wired() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let root = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = root["id"].as_str().unwrap().to_string();

        // 类型表是静态数据，不联网
        let types = host.dispatch("mods_types", json!({})).unwrap();
        let types = types.as_array().unwrap();
        assert!(types.iter().any(|t| t["id"] == json!("worldgen")));
        assert!(types
            .iter()
            .all(|t| !t["label"].as_str().unwrap_or("").is_empty()));
        assert!(types.iter().any(|t| t["cfCategoryId"] == json!(6814)));

        // 目标版本：能解析游戏版本与 mods 目录
        let target = host
            .dispatch(
                "mods_target",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(target["gameVersion"], json!("1.21.4"));
        assert_eq!(target["loader"], json!("vanilla"));
        assert!(
            target["modsDir"]
                .as_str()
                .unwrap()
                .replace('\\', "/")
                .ends_with("mods"),
            "got {}",
            target["modsDir"]
        );

        // 不存在的版本要报错，不能 panic
        assert!(host
            .dispatch(
                "mods_target",
                json!({ "rootId": root_id, "versionId": "9.9.9" }),
            )
            .is_err());

        // 插件管理页能看见 mod-download
        let plugins = host.dispatch("plugins_list", json!({})).unwrap();
        let md = plugins
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == json!("mod-download"))
            .expect("mod-download plugin should be registered");
        assert_eq!(md["enabled"], json!(true));
        assert_eq!(md["name"], json!("模组下载"));
    }

    #[test]
    fn mods_install_without_version_manager_reports_clearly() {
        let (_dir, data, mc) = env();
        // 最小组合：版本管理被停用，所以没有下载服务
        let host = Host::boot(&data, true).unwrap();
        let root = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = root["id"].as_str().unwrap().to_string();

        // 搜索/详情所需的服务仍然在
        assert!(host.dispatch("mods_types", json!({})).is_ok());

        let err = host
            .dispatch(
                "mods_install",
                json!({
                    "rootId": root_id,
                    "versionId": "1.21.4",
                    "source": "modrinth",
                    "projectId": "AANobbMI",
                    "fileId": "abc",
                    "overwrite": false
                }),
            )
            .unwrap_err();
        assert!(err.contains("下载服务不可用"), "got {err}");
    }

    #[test]
    fn launch_and_crash_commands_are_wired_for_multiple_sessions() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let root = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = root["id"].as_str().unwrap().to_string();

        // 没启动过：没有会话、状态是 idle、会话 id 是 0
        let states = host.dispatch("launch_states", json!({})).unwrap();
        assert_eq!(states, json!([]));
        let state = host.dispatch("launch_state", json!({})).unwrap();
        assert_eq!(state["phase"], "idle");
        assert_eq!(state["sessionId"], 0);

        // 「结束全部」在没有会话时是空操作，不能报错（界面已经没有结束按钮，
        // 但命令与 CLI 都还在用）
        assert_eq!(host.dispatch("launch_stop", json!({})).unwrap(), json!([]));
        assert_eq!(
            host.dispatch("launch_stop", json!({ "sessionId": 7 })).unwrap(),
            json!([])
        );

        // 崩溃报告按会话取；不存在的会话不能返回别人的报告
        let report = host.dispatch("crash_report", json!({})).unwrap();
        assert_eq!(report["detected"], false);
        let report = host
            .dispatch("crash_report", json!({ "sessionId": 7 }))
            .unwrap();
        assert_eq!(report["detected"], false);

        // 启动前检查：一个账号都没有时必须**返回 block**（而不是报命令错误），
        // 界面拿它弹窗并给出「去登录」。
        let check = host
            .dispatch(
                "launch_check",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(check["ok"], false);
        assert_eq!(check["blocks"][0]["kind"], "no-account");
        assert!(check["blocks"][0]["message"].as_str().unwrap().contains("账号"));
    }

    /// 启动进度相关命令的参数都得能被界面按 camelCase 传进来。
    ///
    /// 这三条是新增的：`launch_cancel` / `launch_console_tail` / `crash_export_zip`。
    /// 没有会话时它们要给出**人话错误**，而不是 panic 或 "missing field"。
    #[test]
    fn launch_progress_commands_are_wired() {
        // 参数解析：camelCase → snake_case
        let cancel: super::SessionArgs =
            serde_json::from_value(json!({ "sessionId": 3 })).unwrap();
        assert_eq!(cancel.session_id, Some(3));
        let tail: super::ConsoleTailArgs =
            serde_json::from_value(json!({ "sessionId": 3, "maxBytes": 4096 })).unwrap();
        assert_eq!(tail.session_id, Some(3));
        assert_eq!(tail.max_bytes, Some(4096));
        let zip: super::CrashZipArgs =
            serde_json::from_value(json!({ "targetDir": "C:/tmp", "sessionId": 1 })).unwrap();
        assert_eq!(zip.target_dir, "C:/tmp");
        assert_eq!(zip.session_id, Some(1));

        // 没有会话时的错误文案
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let err = host
            .dispatch("launch_cancel", json!({ "sessionId": 9 }))
            .unwrap_err();
        assert!(err.contains("不存在"), "got {err}");
        let err = host.dispatch("launch_console_tail", json!({})).unwrap_err();
        assert!(err.contains("没有日志"), "got {err}");
        let err = host
            .dispatch(
                "crash_export_zip",
                json!({ "targetDir": data.to_string_lossy() }),
            )
            .unwrap_err();
        assert!(err.contains("没有可导出的日志"), "got {err}");
        // 缺 targetDir 是参数错误，不能落到文件系统上
        assert!(host.dispatch("crash_export_zip", json!({})).is_err());
    }

    #[test]
    fn ranking_args_stay_backward_compatible() {        // 界面老调用只传 `{ force }`：新增的可选参数不能让它解析失败
        let args: super::ModRankingArgs = serde_json::from_value(json!({ "force": true })).unwrap();
        assert!(args.force);
        assert!(args.type_id.is_none() && args.page.is_none());
        assert!(args.game_version.is_none() && args.loader.is_none());

        // 完全不传参数也得能解析（默认 force=false）
        let args: super::ModRankingArgs = serde_json::from_value(json!({})).unwrap();
        assert!(!args.force);

        // 老调用还会带 `rootId` / `versionId`：这两个字段已经删了，多传不能报错，
        // 更不能又被拿来收窄排行（初始列表 = 无条件下载量排行）。
        let args: super::ModRankingArgs = serde_json::from_value(json!({
            "rootId": "r1",
            "versionId": "1.21.4-fabric",
            "typeId": "worldgen",
            "gameVersion": "1.21.4",
            "loader": "fabric",
            "page": 2
        }))
        .unwrap();
        assert_eq!(args.page, Some(2));
        assert_eq!(args.type_id.as_deref(), Some("worldgen"));
        assert_eq!(args.game_version.as_deref(), Some("1.21.4"));
    }

    /// 手写一个 stored（method 0）的 zip：本地头 + 内容 + 中央目录 + EOCD。
    ///
    /// 端到端测「读 jar 元数据 → 取图标」必须有个真 zip；测试不联网也不引测试包，
    /// 所以自己拼一个最小可读的（思路同 `kaleid-core-plugins` 里 jar 的测试）。
    fn stored_jar(files: &[(&str, &[u8])]) -> Vec<u8> {
        fn u16le(out: &mut Vec<u8>, value: u16) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        fn u32le(out: &mut Vec<u8>, value: u32) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for byte in data {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xedb8_8320 & mask);
                }
            }
            !crc
        }

        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in files {
            let crc = crc32(data);
            let offset = out.len() as u32;
            u32le(&mut out, 0x0403_4b50);
            u16le(&mut out, 20);
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u32le(&mut out, crc);
            u32le(&mut out, data.len() as u32);
            u32le(&mut out, data.len() as u32);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            u32le(&mut central, 0x0201_4b50);
            u16le(&mut central, 20);
            u16le(&mut central, 20);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u32le(&mut central, crc);
            u32le(&mut central, data.len() as u32);
            u32le(&mut central, data.len() as u32);
            u16le(&mut central, name.len() as u16);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u32le(&mut central, 0);
            u32le(&mut central, offset);
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        u32le(&mut out, 0x0605_4b50);
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u16le(&mut out, files.len() as u16);
        u16le(&mut out, files.len() as u16);
        u32le(&mut out, cd_size);
        u32le(&mut out, cd_offset);
        u16le(&mut out, 0);
        out
    }

    #[test]
    fn mods_update_and_icon_commands_are_wired() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let root = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = root["id"].as_str().unwrap().to_string();

        let dirs = host
            .dispatch(
                "content_dirs",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        let mods_dir = std::path::PathBuf::from(dirs["modsDir"].as_str().unwrap());
        std::fs::create_dir_all(&mods_dir).unwrap();

        // 一个带元数据和图标的真 zip
        let jar = stored_jar(&[
            (
                "fabric.mod.json",
                br#"{"schemaVersion":1,"id":"mymod","name":"My Mod","version":"1.2.3","authors":["Alice"],"icon":"assets/mymod/icon.png"}"#.as_slice(),
            ),
            ("assets/mymod/icon.png", b"\x89PNG\r\n\x1a\nICON"),
        ]);
        std::fs::write(mods_dir.join("mymod-1.0.0.jar"), &jar).unwrap();
        // 一个坏文件：一个文件读不出来不能让整次更新检查失败
        std::fs::write(mods_dir.join("broken-1.0.jar"), b"not a zip").unwrap();

        // 列表：元数据来自 jar，读不出来的回落文件名
        let list = host
            .dispatch(
                "content_mods_list",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        let entries = list.as_array().unwrap();
        let mymod = entries
            .iter()
            .find(|m| m["fileName"] == json!("mymod-1.0.0.jar"))
            .expect("mymod 应当在列表里");
        assert_eq!(mymod["metaSource"], json!("fabric.mod.json"));
        assert_eq!(mymod["hasIcon"], json!(true));
        assert_eq!(mymod["name"], json!("My Mod"));
        assert_eq!(mymod["version"], json!("1.2.3"));
        let broken = entries
            .iter()
            .find(|m| m["fileName"] == json!("broken-1.0.jar"))
            .expect("坏文件也要列出来");
        assert_eq!(broken["metaSource"], json!("filename"));
        assert_eq!(broken["hasIcon"], json!(false));

        // 图标：base64 data URL；没有图标的文件是明确错误
        let icon = host
            .dispatch(
                "content_mod_icon",
                json!({ "rootId": root_id, "versionId": "1.21.4", "fileName": "mymod-1.0.0.jar" }),
            )
            .unwrap();
        assert!(
            icon["dataUrl"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,"),
            "got {icon}"
        );
        assert_eq!(icon["mime"], json!("image/png"));
        assert!(
            host.dispatch(
                "content_mod_icon",
                json!({ "rootId": root_id, "versionId": "1.21.4", "fileName": "broken-1.0.jar" }),
            )
            .is_err(),
            "没有图标的模组应当报错"
        );

        // 更新检查：假 jar 在真实站点上必然对不上；能联网时应进 unknown，
        // 断网时就只剩 warnings——两种情况都不许整次报错。
        let report = host
            .dispatch(
                "mods_updates_check",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(report["updates"], json!([]), "got {report}");
        assert!(report["checkedAt"].as_u64().unwrap_or(0) > 0, "got {report}");
        let unknown = report["unknown"].as_array().unwrap();
        let warnings = report["warnings"].as_array().unwrap();
        assert!(
            !unknown.is_empty() || !warnings.is_empty(),
            "认不出来源时至少要给个说法：{report}"
        );

        // 应用更新：参数能解析，非法输入给中文错误（不联网）
        let base = json!({
            "rootId": root_id,
            "versionId": "1.21.4",
            "update": {
                "fileName": "mymod-1.0.0.jar",
                "modId": "mymod",
                "source": "modrinth",
                "projectId": "mymod",
                "currentVersion": "1.2.3",
                "latestVersion": "2.0.0",
                "latestFileName": "mymod-2.0.0.jar",
                "downloadUrl": "",
                "size": 0,
                "sha1": null,
                "pageUrl": null
            }
        });
        let err = host.dispatch("mods_apply_update", base.clone()).unwrap_err();
        assert!(err.contains("下载地址"), "got {err}");

        let mut bad_name = base.clone();
        bad_name["update"]["latestFileName"] = json!("notes.txt");
        let err = host.dispatch("mods_apply_update", bad_name).unwrap_err();
        assert!(err.contains("不是 jar"), "got {err}");
    }

    #[test]
    fn content_commands_read_the_isolated_version_folder() {
        let (_dir, data, mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let root = host
            .dispatch("roots_add", json!({ "path": mc.to_string_lossy() }))
            .unwrap();
        let root_id = root["id"].as_str().unwrap().to_string();

        // 隔离默认开启，目录落在 versions/<id>/ 下面
        let dirs = host
            .dispatch(
                "content_dirs",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(dirs["isolated"], json!(true), "got {dirs}");
        let mods_dir = dirs["modsDir"].as_str().unwrap().to_string();
        assert!(
            mods_dir
                .replace('\\', "/")
                .ends_with("versions/1.21.4/mods"),
            "got {mods_dir}"
        );

        // 空目录：四个列表都是空数组而不是错误
        for cmd in [
            "content_mods_list",
            "content_packs_list",
            "content_saves_list",
            "content_logs_list",
        ] {
            let value = host
                .dispatch(cmd, json!({ "rootId": root_id, "versionId": "1.21.4" }))
                .unwrap();
            assert_eq!(value, json!([]), "{cmd} 应为空数组");
        }

        // 放一个模组 → 列表里出现；停用 → 改名成 .jar.disabled
        std::fs::create_dir_all(&mods_dir).unwrap();
        let mod_path = std::path::Path::new(&mods_dir).join("sodium-0.6.9.jar");
        std::fs::write(&mod_path, b"jar").unwrap();
        let list = host
            .dispatch(
                "content_mods_list",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["enabled"], json!(true));
        // 显示名来自中文名词表（`sodium` → `钠`），不带扩展名
        let shown = list[0]["displayName"].as_str().unwrap();
        assert!(!shown.is_empty() && !shown.contains(".jar"), "got {shown}");

        let toggled = host
            .dispatch(
                "content_mods_set_enabled",
                json!({
                    "rootId": root_id,
                    "versionId": "1.21.4",
                    "file": "sodium-0.6.9.jar",
                    "enabled": false
                }),
            )
            .unwrap();
        assert_eq!(toggled[0]["enabled"], json!(false));
        assert!(std::path::Path::new(&mods_dir)
            .join("sodium-0.6.9.jar.disabled")
            .is_file());
        assert!(!mod_path.exists());

        // 日志：写一个 latest.log，读尾部
        let logs_dir = dirs["logsDir"].as_str().unwrap();
        std::fs::create_dir_all(logs_dir).unwrap();
        std::fs::write(
            std::path::Path::new(logs_dir).join("latest.log"),
            b"hello log",
        )
        .unwrap();
        let logs = host
            .dispatch(
                "content_logs_list",
                json!({ "rootId": root_id, "versionId": "1.21.4" }),
            )
            .unwrap();
        assert_eq!(logs.as_array().unwrap().len(), 1);
        assert_eq!(logs[0]["isLatest"], json!(true));
        let tail = host
            .dispatch(
                "content_logs_read",
                json!({
                    "rootId": root_id,
                    "versionId": "1.21.4",
                    "file": "latest.log"
                }),
            )
            .unwrap();
        assert_eq!(tail["text"], json!("hello log"));
        assert_eq!(tail["truncated"], json!(false));

        // 非法文件名是错误，不是"读到别的地方"
        let err = host
            .dispatch(
                "content_logs_read",
                json!({
                    "rootId": root_id,
                    "versionId": "1.21.4",
                    "file": "../options.txt"
                }),
            )
            .unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn java_commands_are_wired() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();

        // 本机扫描结果（只断言形状：没有 Java 的机器上就是空数组）
        let list = host.dispatch("java_list", json!({})).unwrap();
        assert!(list.is_array(), "got {list}");

        // 指定一个假 JDK 目录：inspect 认它，但没有 release 文件 → valid = false
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("java.exe");
        std::fs::write(&exe, b"x").unwrap();
        let inspected = host
            .dispatch("java_inspect", json!({ "path": exe.to_string_lossy() }))
            .unwrap();
        assert_eq!(inspected["valid"], json!(false));
        assert_eq!(inspected["path"], json!(exe.to_string_lossy().to_string()));

        // 手动指定路径优先：pick 直接返回它
        std::fs::write(
            dir.path().join("release"),
            "JAVA_VERSION=\"21.0.2\"\nIMPLEMENTOR=\"Eclipse Adoptium\"\nOS_ARCH=\"x86_64\"\n",
        )
        .unwrap();
        let picked = host
            .dispatch(
                "java_pick",
                json!({ "major": 21, "javaPath": exe.to_string_lossy() }),
            )
            .unwrap();
        assert_eq!(picked["major"], json!(21));
        assert_eq!(picked["source"], json!("custom"));
    }

    #[test]
    fn content_commands_report_missing_plugin() {
        let (_dir, data, mc) = env();
        // 合成一份缺少 game-content 的测试组合：验证缺失服务提示，不是停用生产必需插件。
        let kernel = Kernel::new();
        register_all(&kernel, &data);
        let specs = standard_preset()
            .into_iter()
            .filter(|spec| spec.id != "game-content")
            .collect::<Vec<_>>();
        kernel.load(&specs).unwrap();

        let root = dispatch(
            &kernel,
            "roots_add",
            json!({ "path": mc.to_string_lossy() }),
        )
        .unwrap();
        let root_id = root["id"].as_str().unwrap().to_string();
        let err = dispatch(
            &kernel,
            "content_mods_list",
            json!({ "rootId": root_id, "versionId": "1.21.4" }),
        )
        .unwrap_err();
        assert!(err.contains("本地内容服务不可用"), "got {err}");
    }

    #[test]
    fn msa_commands_are_wired() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();

        // client_id 是编译期注入的：配了就必须是配置完成的样子，没配就必须给提示
        let config = host.dispatch("msa_config", json!({})).unwrap();
        if config["configured"] == json!(true) {
            assert!(config["hint"].is_null(), "已配置不该再提示：{config}");
        } else {
            assert!(
                config["hint"]
                    .as_str()
                    .unwrap()
                    .contains("KALEID_MSA_CLIENT_ID"),
                "未配置要说清填哪儿：{config}"
            );
        }

        // 没有发起过登录：轮询要报一句能照着做的话，而不是 500
        let err = host.dispatch("msa_poll", json!({})).unwrap_err();
        assert!(err.contains("添加正版账号"), "got {err}");

        // 取消永远成功（界面关弹窗时会调它）
        assert!(host.dispatch("msa_cancel", json!({})).is_ok());

        // 还没登录过任何正版账号
        let states = host.dispatch("msa_states", json!({})).unwrap();
        assert_eq!(states, json!([]));

        // 退出登录没有独立命令：删账号时顺手清令牌（见下一条用例），
        // 界面不需要"只退出、不删账号"这个动作。
    }

    #[test]
    fn msa_commands_report_missing_plugin() {
        let (_dir, data, _mc) = env();
        // 合成一份缺少 msa-login 的测试组合：验证缺失服务提示，不是停用生产必需插件。
        let kernel = Kernel::new();
        register_all(&kernel, &data);
        let specs = standard_preset()
            .into_iter()
            .filter(|spec| spec.id != "msa-login")
            .collect::<Vec<_>>();
        kernel.load(&specs).unwrap();

        let err = dispatch(&kernel, "msa_config", json!({})).unwrap_err();
        assert!(err.contains("微软登录服务不可用"), "got {err}");
    }

    #[test]
    fn removing_an_account_also_drops_its_saved_token() {
        let (_dir, data, _mc) = env();
        let host = Host::boot(&data, false).unwrap();
        let account = host
            .dispatch("accounts_apply_offline", json!({ "name": "Player_123" }))
            .unwrap();
        let id = account["id"].as_str().unwrap().to_string();
        // 离线账号没有令牌文件，删除仍然要成功（顺手清理不该变成阻塞）
        host.dispatch("accounts_remove", json!({ "id": id }))
            .unwrap();
        assert_eq!(
            host.dispatch("accounts_active", json!({})).unwrap(),
            json!(null)
        );
    }
}
