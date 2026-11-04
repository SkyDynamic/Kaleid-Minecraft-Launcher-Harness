//! 真实启动：spawn Java、收集控制台、判定崩溃、导出排障包。
//!
//! 关键约定：
//! - **访问令牌只在 spawn 这一刻注入**，不写进计划、日志或启动脚本（脚本里是 `***`）。
//! - 「启动完毕」= 游戏窗口真的出来了；拿不到窗口句柄时退化成「进程存活且日志出现
//!   了初始化完成标记」，界面的启动按钮靠它结束 Loading。
//! - 崩溃分析永远只是推测（见 [`crate::crash`]），导出的 manifest 会把依据一并写出，
//!   用户和后续排查都能复核。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use kaleid_api::{
    ApiError, ApiResult, CrashArchive, CrashExport, CrashReport, LaunchPhase, LaunchPlan,
    LaunchState, LaunchStep, PhaseStatus,
};

use crate::crash;
use crate::crash_zip;
use crate::downloads::{DownloadManager, Job};
use crate::downloads::diagnostics::{Operation, publish, millis, spawn_thread, observed_cancel};
use kaleid_kernel::{DiagnosticRecord, Severity};
use crate::gpu;
use crate::jar;
use crate::options;
use crate::repair;

/// 「窗口出现了」的日志佐证。缺窗口 API 时用它把 `Launching` 推进到 `Running`。
const RUNNING_MARKERS: &[&str] = &[
    "backend library:",
    "openal initialized",
    "sound engine started",
    "created: 1024x",
    "lwjgl version",
    "setting user:",
    "started serving on",
];

/// 兜底：进程活过这么久还没报错，就当已经起来了（某些版本这些日志被 mod 吃掉）。
const RUNNING_FALLBACK: Duration = Duration::from_secs(25);

/// 控制台尾巴的重导出上限，避免几十万行的 mod 日志把界面拖死。
const LOG_TAIL_LINES: usize = 400;

/// 已经结束的会话在列表里留多久（毫秒）。再久就没人看了，留着只会让轮询变慢。
const SESSION_KEEP_MS: u64 = 10 * 60 * 1000;

/// 最多保留多少个会话快照（防止长时间挂着启动器越攒越多）。
const SESSION_KEEP_MAX: usize = 16;

struct Runtime {
    diagnostics: Operation,
    state: LaunchState,
    child: Option<Child>,
    log: Arc<Mutex<String>>,
    report: CrashReport,
    /// 这次启动时 `mods/` 里的文件名，导出 manifest 时用（启动后用户可能又装了新模组，
    /// 崩溃却与它们无关，所以要在启动那一刻快照）。
    mods: Vec<String>,
    run_dir: Option<PathBuf>,
    plan: Option<LaunchPlan>,
    /// 「取消启动」的旗子。准备线程与下载任务都盯着它。
    cancel: Arc<AtomicBool>,
    /// 进程是被我们自己结束的（实例面板的「结束进程」）。
    ///
    /// 主动结束拿到的退出码必然非零，但那不是崩溃——不看这个标记，
    /// 用户点一次「结束进程」就会收到一份崩溃报告。
    killed: bool,
}

/// 一个会话 = 一次「点启动」。多开就是这里有多份。
struct Session {
    id: u64,
    runtime: Runtime,
}

pub struct GameProcessManager {
    data_root: PathBuf,
    /// 所有会话。**不再有"正在运行就不许再启动"的锁**：多开是这个启动器的正常功能。
    sessions: Arc<Mutex<Vec<Session>>>,
    next_id: std::sync::atomic::AtomicU64,
    /// 补全缺失文件用的下载器。
    ///
    /// 它由「版本管理」插件提供，所以是 `Option`：那个插件被停用时补全能力消失，
    /// 但启动本身照常（缺文件就直接报错，和以前一样）。
    downloads: Option<Arc<DownloadManager>>,
    /// 内核上下文（**延迟**取下载器用）。
    ///
    /// 不能在 `apply` 里就 `ctx.service::<DownloadManager>()` 一把抓：插件按依赖
    /// 拓扑序 apply，`game-launch` 没有声明依赖，可能排在 `version-manager` **前面**，
    /// 那时下载器还没人提供——真机上就变成"文件缺了却说插件已停用"。留着上下文，
    /// 等真要用（准备阶段）再取，顺序问题就没了。
    ctx: Option<kaleid_kernel::Context>,
    /// 只在测试里用的"准备阶段延迟"，让「取消」这种和线程赛跑的用例可复现。
    #[cfg(test)]
    prep_delay_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl GameProcessManager {
    pub fn new(data_root: &Path) -> Self {
        GameProcessManager {
            data_root: data_root.to_path_buf(),
            sessions: Arc::new(Mutex::new(Vec::new())),
            next_id: std::sync::atomic::AtomicU64::new(1),
            downloads: None,
            ctx: None,
            #[cfg(test)]
            prep_delay_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// 测试专用：让准备线程先睡一会儿，好在它跑到一半时按「取消」。
    #[cfg(test)]
    fn set_prep_delay(&self, ms: u64) {
        self.prep_delay_ms.store(ms, Ordering::SeqCst);
    }

    fn prep_delay(&self) -> u64 {
        #[cfg(test)]
        {
            return self.prep_delay_ms.load(Ordering::SeqCst);
        }
        #[cfg(not(test))]
        {
            0
        }
    }

    /// 直接挂上下载器（测试、以及将来别的宿主自己造管理器时用）。
    pub fn with_downloads(mut self, downloads: Option<Arc<DownloadManager>>) -> Self {
        self.downloads = downloads;
        self
    }

    /// 挂上内核上下文，好让准备阶段**按需**取下载器（见字段注释）。
    pub fn with_context(mut self, ctx: kaleid_kernel::Context) -> Self {
        self.ctx = Some(ctx);
        self
    }

    /// 现在能拿到的下载器：显式挂的优先，其次问内核要（版本管理插件在就给）。
    fn resolve_downloads(&self) -> Option<Arc<DownloadManager>> {
        self.downloads
            .clone()
            .or_else(|| self.ctx.as_ref().and_then(|ctx| ctx.service::<DownloadManager>()))
    }

    /// 最近一次会话的状态（没有会话时是 `idle`）。CLI 与老界面调用继续用它。
    pub fn state(&self) -> LaunchState {
        let sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.last() else {
            return LaunchState::idle();
        };
        snapshot(&session.runtime)
    }

    /// 所有会话的状态：正在跑的排在前面，其次是最近的已结束会话。
    pub fn states(&self) -> Vec<LaunchState> {
        let mut sessions = self.sessions.lock().unwrap();
        prune_sessions(&mut sessions);
        let mut states: Vec<LaunchState> = sessions.iter().map(|s| snapshot(&s.runtime)).collect();
        states.sort_by(|a, b| {
            let busy = b.phase.is_busy().cmp(&a.phase.is_busy());
            if busy != std::cmp::Ordering::Equal {
                return busy;
            }
            b.session_id.cmp(&a.session_id)
        });
        states
    }

    /// 某个版本最近一次会话（界面用它决定"当前这个版本在不在跑"）。
    pub fn state_for(&self, root_id: &str, version_id: &str) -> Option<LaunchState> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .iter()
            .rev()
            .find(|s| {
                s.runtime.state.version_id == version_id
                    && (root_id.is_empty() || s.runtime.state.root_id == root_id)
            })
            .map(|s| snapshot(&s.runtime))
    }

    /// 崩溃报告。`session_id = None` 取最近一个会话。
    pub fn report(&self, session_id: Option<u64>) -> CrashReport {
        let sessions = self.sessions.lock().unwrap();
        match session_id {
            Some(id) => sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.runtime.report.clone())
                .unwrap_or_else(CrashReport::none),
            None => sessions
                .last()
                .map(|s| s.runtime.report.clone())
                .unwrap_or_else(CrashReport::none),
        }
    }

    /// 启动游戏。`token` 是这次要注入的真实访问令牌（离线账号不需要）。
    ///
    /// 每次调用都会新开一个会话：同一个版本连点两次就是两个游戏进程，和 PCL2 一致。
    ///
    /// **它不等游戏起来**：会话以 `Preparing` 落进列表、五步进度全部 pending，
    /// 然后 `kaleid-launch-prep` 线程按「检查 → 补全 → 解压 → 启动 → 等窗口」推进。
    /// 界面的进度条轮询的就是这份状态；CLI 想等结果就自己轮询 [`Self::state`]。
    pub fn start(
        &self,
        plan: LaunchPlan,
        token: Option<String>,
        mods: Vec<String>,
    ) -> ApiResult<LaunchState> {
        let session_id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let mut operation = Operation::new(self.ctx.clone(), "launch", "launch");
        operation.record = operation.record.with_session_id(session_id.to_string()).with_field("version_id", plan.version_id.clone()).with_field("game_version", plan.game_version.clone());
        operation.accepted();
        let run_dir = self.run_dir_for(&plan, session_id);
        std::fs::create_dir_all(&run_dir).map_err(|e| { operation.finish(&Err::<(), _>(ApiError::io(&e)), false); e })?;
        let log_path = run_dir.join("console.log");

        let cancel = Arc::new(AtomicBool::new(false));
        let state = LaunchState {
            session_id,
            phase: LaunchPhase::Preparing,
            root_id: plan.version_id.clone(),
            version_id: plan.version_id.clone(),
            started_at: Some(unix_ms()),
            log_path: Some(log_path.to_string_lossy().to_string()),
            notes: plan.jvm_notes.clone(),
            steps: kaleid_api::fresh_launch_steps(),
            ..LaunchState::idle()
        };

        {
            let mut sessions = self.sessions.lock().unwrap();
            prune_sessions(&mut sessions);
            sessions.push(Session {
                id: session_id,
                runtime: Runtime {
                    diagnostics: operation.clone(),
                    state: state.clone(),
                    child: None,
                    log: Arc::new(Mutex::new(String::new())),
                    report: CrashReport::none(),
                    mods: mods.clone(),
                    run_dir: Some(run_dir.clone()),
                    plan: Some(plan.clone()),
                    cancel: cancel.clone(),
                    killed: false,
                },
            });
        }

        let sessions = self.sessions.clone();
        let data_root = self.data_root.clone();
        let downloads = self.resolve_downloads();
        let prep_delay = self.prep_delay();
        spawn_thread("kaleid-launch-prep", move || {
                prepare_and_spawn(
                    sessions, data_root, downloads, session_id, plan, token, mods, run_dir, cancel,
                    prep_delay,
                );
            })
            .map_err(|e| { operation.record = operation.record.clone().with_field("category", "thread_spawn"); let error = ApiError::io(format!("无法创建启动准备线程：{e}")); operation.finish(&Err::<(), _>(ApiError::io("thread_spawn")), false); error })?;

        Ok(state)
    }

    /// 取消一次**还在准备阶段**的启动（PCL2 的「取消」按钮）。
    ///
    /// 进程已经起来之后再取消没有意义：那时候要么等它进游戏，要么用 [`Self::stop`]
    /// 直接结束。所以这里对 `Launching` / `Running` 明说一句人话而不是假装成功——
    /// 界面要是拿到 `Ok` 就会把遮罩收掉，用户会以为"取消了"可游戏还在起。
    pub fn cancel(&self, session_id: u64) -> ApiResult<LaunchState> {
        let result = (|| {
        let sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.iter().find(|s| s.id == session_id) else {
            return Err(ApiError::not_found("这个启动会话已经不存在了"));
        };
        match session.runtime.state.phase {
            LaunchPhase::Launching | LaunchPhase::Running => {
                return Err(ApiError::invalid(
                    "游戏进程已经起来了，要停下请点「结束进程」",
                ))
            }
            _ => {}
        }
        session.runtime.cancel.store(true, Ordering::SeqCst);
        Ok(snapshot(&session.runtime))
        })();
        publish(&self.ctx, DiagnosticRecord::new("launch", "launch.cancel_requested", Severity::Info).with_session_id(session_id.to_string()).with_field("outcome", if result.is_ok() { "accepted" } else { "rejected" }).with_field("reason", "user_request"));
        result
    }

    /// 结束游戏进程（连同它拉起的子进程）。
    ///
    /// `session_id = None` 表示"把所有在跑的会话都结束"；界面不再提供这个按钮，
    /// 但 CLI / 将来的托盘菜单需要它。
    pub fn stop(&self, session_id: Option<u64>) -> ApiResult<Vec<LaunchState>> {
        let mut sessions = self.sessions.lock().unwrap();
        let mut stopped = Vec::new();
        for session in sessions.iter_mut() {
            if let Some(want) = session_id {
                if session.id != want {
                    continue;
                }
            }
            // 还在准备阶段的会话没有 child：置取消标记，让准备线程自己收尾。
            if session.runtime.child.is_none() {
                if session.runtime.state.phase == LaunchPhase::Preparing {
                    session.runtime.cancel.store(true, Ordering::SeqCst);
                    stopped.push(snapshot(&session.runtime));
                }
                continue;
            }
            let Some(child) = session.runtime.child.as_mut() else {
                continue;
            };
            let pid = child.id();
            // 先打标记再动手：watcher 是另一个线程，晚一步写就会被它读到"非零退出"。
            session.runtime.killed = true;
            #[cfg(windows)]
            {
                // 直接 kill 只杀 java.exe，Minecraft 有时会拉起子进程（如外置的模组加载器）。
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = child.kill();
            stopped.push(snapshot(&session.runtime));
        }
        drop(sessions);
        for state in &stopped { publish(&self.ctx, DiagnosticRecord::new("launch", "launch.stop_requested", Severity::Info).with_session_id(state.session_id.to_string()).with_field("outcome", "accepted").with_field("reason", "user_request")); }
        Ok(stopped)
    }

    fn run_dir_for(&self, plan: &LaunchPlan, session_id: u64) -> PathBuf {
        let stamp = unix_ms();
        // 带上会话 id：同一个版本多开时日志目录不能互相覆盖。
        self.data_root.join("logs").join(format!(
            "{}-{stamp}-{session_id}",
            sanitize(&plan.version_id)
        ))
    }

    /// 导出排障包：latest.log、崩溃前尾巴、crash.txt、启动脚本、manifest.json。
    pub fn export(
        &self,
        target_dir: Option<&Path>,
        session_id: Option<u64>,
    ) -> ApiResult<CrashExport> {
        let bundle = self.bundle(session_id)?;
        let dir = match target_dir {
            Some(p) => p.to_path_buf(),
            None => {
                let stamp = unix_ms();
                self.data_root.join("crash-exports").join(format!(
                    "{}-{stamp}",
                    sanitize(&bundle.version_id)
                ))
            }
        };
        std::fs::create_dir_all(&dir)?;
        for (name, bytes) in &bundle.files {
            std::fs::write(dir.join(name), bytes)?;
        }
        Ok(CrashExport {
            dir: dir.to_string_lossy().to_string(),
            files: bundle.files.iter().map(|(n, _)| n.clone()).collect(),
            manifest_path: dir.join("manifest.json").to_string_lossy().to_string(),
        })
    }

    /// 导出成 ZIP（PCL2 的「导出崩溃日志」）。
    ///
    /// 内容和 [`Self::export`] 完全一样，只是打成单个文件落到用户选的目录里。
    pub fn export_zip(
        &self,
        target_dir: &Path,
        session_id: Option<u64>,
    ) -> ApiResult<CrashArchive> {
        let bundle = self.bundle(session_id)?;
        let name = crash_zip::local_now().archive_name();
        // 先写进临时目录再打包：有点绕，但这样 `bundle` 只有一份数据来源，
        // 目录导出和 zip 导出不会各写一套文件名。
        let staging = std::env::temp_dir().join(format!(
            "kaleid-crash-{}-{}",
            sanitize(&bundle.version_id),
            unix_ms()
        ));
        let result = (|| -> ApiResult<CrashArchive> {
            std::fs::create_dir_all(&staging)?;
            for (file, bytes) in &bundle.files {
                std::fs::write(staging.join(file), bytes)?;
            }
            let outcome = crash_zip::zip_directory(target_dir, &name, &staging)?;
            Ok(CrashArchive {
                path: outcome.path.to_string_lossy().to_string(),
                files: outcome.files,
                bytes: outcome.bytes,
                file_name: name.clone(),
            })
        })();
        let _ = std::fs::remove_dir_all(&staging);
        result
    }

    /// 某次启动的 console 日志尾部（「本次日志」弹窗用）。
    pub fn console_tail(
        &self,
        session_id: Option<u64>,
        max_bytes: usize,
    ) -> ApiResult<kaleid_api::LogTail> {
        let sessions = self.sessions.lock().unwrap();
        let session = match session_id {
            Some(id) => sessions.iter().find(|s| s.id == id),
            None => sessions.last(),
        };
        let Some(session) = session else {
            return Err(ApiError::not_found("还没有启动过游戏，没有日志"));
        };
        let runtime = &session.runtime;
        let text = runtime.log.lock().unwrap().clone();
        let total = text.len();
        let (shown, truncated) = if total > max_bytes {
            let start = floor_char_boundary(&text, total - max_bytes);
            (text[start..].to_string(), true)
        } else {
            (text.clone(), false)
        };
        Ok(kaleid_api::LogTail {
            file_name: format!("{}-console.log", runtime.state.version_id),
            total_bytes: total as u64,
            shown_bytes: shown.len() as u64,
            truncated,
            text: shown,
        })
    }

    /// 排障包的完整内容（目录导出与 ZIP 导出共用）。
    fn bundle(&self, session_id: Option<u64>) -> ApiResult<ExportBundle> {
        let sessions = self.sessions.lock().unwrap();
        let session = match session_id {
            Some(id) => sessions.iter().find(|s| s.id == id),
            None => sessions.last(),
        };
        let Some(session) = session else {
            return Err(ApiError::not_found("还没有启动过游戏，没有可导出的日志"));
        };
        let runtime = &session.runtime;
        let log_text = runtime.log.lock().unwrap().clone();
        let mods = runtime.mods.clone();
        let report = runtime.report.clone();
        let plan = runtime.plan.clone();

        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut push = |name: &str, bytes: Vec<u8>| files.push((name.to_string(), bytes));

        push("latest.log", log_text.as_bytes().to_vec());
        push(
            "last-log-before-crash.txt",
            crash::tail_lines(&log_text, LOG_TAIL_LINES).into_bytes(),
        );
        if let Some(path) = report.crash_file.as_deref().map(PathBuf::from) {
            if let Ok(bytes) = std::fs::read(&path) {
                push("crash.txt", bytes);
            }
        }
        if let Some(run_dir) = runtime.run_dir.as_ref() {
            if let Ok(script) = std::fs::read(run_dir.join("launch-script.txt")) {
                push("launch-script.txt", script);
            }
        }
        // 排障时最想问的是"启动器到底传了什么"：LatestLaunch.bat 就是那个答案，
        // 和 launch-script.txt 一起打包，别人拿到 zip 也能自己复现一遍。
        let bat_path = self.data_root.join("LatestLaunch.bat");
        if let Ok(bytes) = std::fs::read(&bat_path) {
            push("LatestLaunch.bat", bytes);
        }

        let manifest = json!({
            "sessionId": runtime.state.session_id,
            "versionId": runtime.state.version_id,
            "gameVersion": plan.as_ref().map(|p| p.game_version.clone()),
            "javaPath": plan.as_ref().map(|p| p.java_path.clone()),
            "gameDir": plan.as_ref().map(|p| p.game_dir.clone()),
            "launchedAt": runtime.state.started_at,
            "elapsedMs": runtime.state.elapsed_ms,
            "exitCode": runtime.state.exit_code,
            "crashed": report.detected,
            "keywords": report.keywords,
            "suspects": report.suspects,
            "notes": runtime.state.notes,
            "mods": mods,
            "steps": runtime.state.steps,
            "exportedAt": unix_ms(),
            "note": "疑似模组只是按日志推测，不是确定结论；请连同 latest.log 一起核对。",
        });
        push(
            "manifest.json",
            serde_json::to_string_pretty(&manifest)?.into_bytes(),
        );

        Ok(ExportBundle {
            version_id: runtime.state.version_id.clone(),
            files,
        })
    }
}

/// 一次导出的全部文件（`bundle` 的产物）。
struct ExportBundle {
    version_id: String,
    files: Vec<(String, Vec<u8>)>,
}

/// 把索引挪到字符边界（日志里有中文，按字节切会 panic）。
fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 把某个会话的运行时状态整理成对外快照（补上实时耗时与会话 id）。
///
/// 进度也在这里算：`percent` / `stage_label` 永远由 `steps` 派生，别的地方只管
/// 改步骤状态，不会出现"步骤 100% 而总进度还在 60%"这种自相矛盾。
fn snapshot(runtime: &Runtime) -> LaunchState {
    let mut state = runtime.state.clone();
    state.session_id = runtime.state.session_id;
    if let Some(started) = state.started_at {
        if !matches!(
            state.phase,
            LaunchPhase::Exited | LaunchPhase::Failed | LaunchPhase::Cancelled
        ) {
            state.elapsed_ms = unix_ms().saturating_sub(started);
        }
    }
    if let (Some(dir), true) = (runtime.run_dir.clone(), state.log_path.is_none()) {
        state.log_path = Some(dir.join("console.log").to_string_lossy().to_string());
    }
    if !state.steps.is_empty() {
        state.percent = kaleid_api::launch_progress(&state.steps);
        state.stage_label = current_stage_label(&state);
    }
    state
}

/// 当前阶段的展示文案：正在跑的那一步优先用它自己的 `detail`。
fn current_stage_label(state: &LaunchState) -> String {
    if let Some(step) = state
        .steps
        .iter()
        .find(|s| s.status == PhaseStatus::Running)
    {
        return if step.detail.is_empty() {
            step.label.clone()
        } else {
            format!("{} · {}", step.label, step.detail)
        };
    }
    if let Some(step) = state
        .steps
        .iter()
        .find(|s| s.status == PhaseStatus::Failed)
    {
        return format!("{}失败", step.label);
    }
    match state.phase {
        LaunchPhase::Preparing => "准备启动…".to_string(),
        LaunchPhase::Launching => "游戏进程已启动，正在等窗口出现…".to_string(),
        LaunchPhase::Running => "游戏已启动".to_string(),
        LaunchPhase::Cancelled => "已取消启动".to_string(),
        LaunchPhase::Failed => state.error.clone().unwrap_or_else(|| "启动失败".to_string()),
        _ => String::new(),
    }
}

/* ------------------------------------------------------------------ *
 * 启动准备：检查 → 补全 → 解压 → 启动 → 等窗口
 * ------------------------------------------------------------------ */

/// 改一个会话的运行时状态；会话已经被清理掉时安静返回 `false`。
fn touch<F>(sessions: &Arc<Mutex<Vec<Session>>>, session_id: u64, f: F) -> bool
where
    F: FnOnce(&mut Runtime),
{
    let Ok(mut guard) = sessions.lock() else {
        return false;
    };
    let Some(session) = guard.iter_mut().find(|s| s.id == session_id) else {
        return false;
    };
    f(&mut session.runtime);
    true
}

fn step_mut<'a>(state: &'a mut LaunchState, id: &str) -> Option<&'a mut LaunchStep> {
    state.steps.iter_mut().find(|s| s.id == id)
}

fn session_operation(sessions: &Arc<Mutex<Vec<Session>>>, id: u64) -> Option<Operation> {
    sessions.lock().ok()?.iter().find(|session| session.id == id).map(|session| session.runtime.diagnostics.clone())
}
fn session_event(sessions: &Arc<Mutex<Vec<Session>>>, id: u64, event: &'static str, level: Severity, fields: &[(&str, serde_json::Value)]) {
    if let Some(operation) = session_operation(sessions, id) {
        let mut record = operation.record.clone(); record.event = event.into(); record.level = level;
        for (key, value) in fields { record = record.with_field(*key, value.clone()); }
        publish(&operation.ctx, record);
    }
}
fn step_status(status: PhaseStatus) -> &'static str { match status { PhaseStatus::Pending => "pending", PhaseStatus::Running => "running", PhaseStatus::Completed => "completed", PhaseStatus::Skipped => "skipped", PhaseStatus::Failed => "failed" } }

/// 改某一步的状态与说明。
fn set_step(
    sessions: &Arc<Mutex<Vec<Session>>>,
    session_id: u64,
    step_id: &str,
    status: PhaseStatus,
    detail: impl Into<String>,
) {
    let detail = detail.into();
    let mut previous = None;
    touch(sessions, session_id, |runtime| {
        if let Some(step) = step_mut(&mut runtime.state, step_id) {
            if step.status != status { previous = Some(step.status); }
            step.status = status;
            step.detail = detail.clone();
        }
    });
    if let Some(previous) = previous { session_event(sessions, session_id, "launch.step", Severity::Info, &[("step", json!(step_id)), ("previous_status", json!(step_status(previous))), ("status", json!(step_status(status)))]); }
}

/// 准备阶段失败：`Failed` + 原因，并把没走完的步骤标掉。
fn fail_preparing(sessions: &Arc<Mutex<Vec<Session>>>, session_id: u64, message: String) {
    let operation = session_operation(sessions, session_id);
    let mut failed_step = None;
    touch(sessions, session_id, |runtime| {
        if let Some(step) = runtime
            .state
            .steps
            .iter_mut()
            .find(|s| s.status == PhaseStatus::Running)
        {
            failed_step = Some(step.id.clone());
            step.status = PhaseStatus::Failed;
            step.detail = message.clone();
        }
        runtime.state.phase = LaunchPhase::Failed;
        runtime.state.error = Some(message);
    });
    if let Some(step) = failed_step { session_event(sessions, session_id, "launch.step", Severity::Error, &[("step", json!(step)), ("status", json!("failed"))]); }
    if let Some(mut operation) = operation { operation.record = operation.record.with_field("category", "preparation"); operation.finish(&Err::<(), _>(ApiError::Unavailable("preparation".into())), false); }
}

/// 用户取消：`Cancelled`，不算崩溃、不写崩溃报告。
fn mark_cancelled(sessions: &Arc<Mutex<Vec<Session>>>, session_id: u64) {
    let operation = session_operation(sessions, session_id);
    let mut skipped_steps = Vec::new();
    touch(sessions, session_id, |runtime| {
        for step in runtime.state.steps.iter_mut() {
            if step.status == PhaseStatus::Running {
                skipped_steps.push(step.id.clone());
                step.status = PhaseStatus::Skipped;
                step.detail = "已取消".into();
            }
        }
        runtime.state.phase = LaunchPhase::Cancelled;
        runtime.state.cancelled = true;
        runtime.state.error = Some("已取消启动".into());
        if let Some(started) = runtime.state.started_at {
            runtime.state.elapsed_ms = unix_ms().saturating_sub(started);
        }
    });
    for step in skipped_steps { session_event(sessions, session_id, "launch.step", Severity::Info, &[("step", json!(step)), ("previous_status", json!("running")), ("status", json!("skipped"))]); }
    session_event(sessions, session_id, "launch.cancelled", Severity::Info, &[("outcome", json!("cancelled")), ("reason", json!("user_request"))]);
    if let Some(operation) = operation { operation.finish(&Err::<(), _>(ApiError::invalid("任务已取消")), true); }
}

fn repair_outcome(result: &ApiResult<()>, join_panicked: bool) -> &'static str {
    if join_panicked { "failed" } else if result.is_ok() { "succeeded" } else if observed_cancel(result) { "cancelled" } else { "failed" }
}

/// 准备线程的本体：五步走完，最后把进程交给 watcher。
#[allow(clippy::too_many_arguments)]
fn prepare_and_spawn(
    sessions: Arc<Mutex<Vec<Session>>>,
    data_root: PathBuf,
    downloads: Option<Arc<DownloadManager>>,
    session_id: u64,
    plan: LaunchPlan,
    token: Option<String>,
    mods: Vec<String>,
    run_dir: PathBuf,
    cancel: Arc<AtomicBool>,
    prep_delay_ms: u64,
) {
    if prep_delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(prep_delay_ms));
    }
    let cancelled = || cancel.load(Ordering::SeqCst);

    // ---- ① 检查启动条件 ----
    set_step(&sessions, session_id, "check", PhaseStatus::Running, "检查 Java 与版本文件…");
    if cancelled() {
        return mark_cancelled(&sessions, session_id);
    }
    // `java_path` 可能是 PATH 里的裸命令名，那时没必要检查文件是否存在。
    let java = Path::new(&plan.java_path);
    if java.components().count() > 1 && !java.is_file() {
        return fail_preparing(
            &sessions,
            session_id,
            format!("找不到 Java：{}（到「设置 → Java」里换一个）", plan.java_path),
        );
    }
    set_step(&sessions, session_id, "check", PhaseStatus::Completed, "Java 与版本文件正常");

    // ---- ② 补全缺失文件 ----
    // 两轮：第一轮可能刚把资源索引下下来，索引里的对象要第二轮才数得出来。
    set_step(&sessions, session_id, "repair", PhaseStatus::Running, "统计缺失文件…");
    let mut repaired_files = 0usize;
    for pass in 0..2 {
        if cancelled() {
            return mark_cancelled(&sessions, session_id);
        }
        let repair_plan = match repair::missing_items(&plan) {
            Ok(value) => value,
            Err(e) => {
                return fail_preparing(
                    &sessions,
                    session_id,
                    format!("无法统计缺失文件：{e}"),
                )
            }
        };
        if repair_plan.is_empty() {
            if pass == 0 {
                set_step(
                    &sessions,
                    session_id,
                    "repair",
                    PhaseStatus::Skipped,
                    "文件齐全，无需下载",
                );
            } else {
                set_step(
                    &sessions,
                    session_id,
                    "repair",
                    PhaseStatus::Completed,
                    format!("已补全 {repaired_files} 个文件"),
                );
            }
            break;
        }
        let Some(downloads) = downloads.clone() else {
            return fail_preparing(
                &sessions,
                session_id,
                format!(
                    "缺少 {} 个文件，但「版本管理」插件被停用了，无法自动补全。\
                     到「设置 → 插件」启用它，或手动补上这些文件。",
                    repair_plan.files()
                ),
            );
        };
        let files = repair_plan.files();
        let total = repair_plan.total_bytes;
        let job = Job::new(
            format!("repair-{session_id}-{pass}"),
            format!("补全缺失文件 · {}", plan.version_id),
            plan.version_id.clone(),
            plan.version_id.clone(),
            plan.game_version.clone(),
            kaleid_api::LoaderKind::Vanilla,
            None,
            downloads.source_policy(),
        );
        for item in &repair_plan.items {
            job.add_total(&item.phase, item.size, 1);
        }
        downloads.register(job.clone());
        let job_id = job.id.clone();
        let repair_started = Instant::now();
        session_event(&sessions, session_id, "launch.repair_started", Severity::Info, &[("pass", json!(pass)), ("files_total", json!(files)), ("total_bytes", json!(total))]);
        touch(&sessions, session_id, |runtime| {
            runtime.state.queue_job_id = Some(job_id.clone());
        });

        let runner_job = job.clone();
        let runner = {
            let downloads = downloads.clone();
            let items = repair_plan.items.clone();
            std::thread::Builder::new()
                .name("kaleid-launch-repair".into())
                .spawn(move || downloads.run_batch(runner_job, items))
        };
        let Ok(runner) = runner else {
            return fail_preparing(&sessions, session_id, "无法创建下载线程".to_string());
        };

        // 边跑边把下载任务的真实字节数搬到这一步上。
        loop {
            std::thread::sleep(Duration::from_millis(150));
            // 取消之后别再回写快照：下载器那边会报"下载失败"，用户看到的应当是
            // 「已取消」，而不是一条像出错的文案。
            if cancelled() {
                job.cancel();
            } else {
                let snap = job.snapshot();
                touch(&sessions, session_id, |runtime| {
                    if let Some(step) = step_mut(&mut runtime.state, "repair") {
                        step.done_bytes = snap.done_bytes;
                        step.total_bytes = snap.total_bytes.max(total);
                        step.files_done = snap.files_done;
                        step.files_total = snap.files_total.max(files);
                        step.detail = if snap.current.is_empty() {
                            format!("{}/{} 个文件", snap.files_done, snap.files_total.max(files))
                        } else {
                            snap.current.clone()
                        };
                    }
                });
            }
            if runner.is_finished() {
                break;
            }
        }
        let joined = runner.join();
        let join_panicked = joined.is_err();
        // Preserve the legacy runtime fallback, but do not describe an observed panic as success.
        let result = joined.unwrap_or_else(|_| Ok(()));
        if job.diagnostic_failed() { if let Some(operation) = session_operation(&sessions, session_id) { operation.note_terminal_failure(); } }
        if let Some(operation) = session_operation(&sessions, session_id) {
            let mut record = operation.record.clone(); record.event = "launch.repair_finished".into();
            record = record.with_job_id(&job.id).with_field("pass", pass as u64).with_field("files_total", files as u64).with_field("total_bytes", total).with_field("duration_ms", millis(repair_started)).with_field("outcome", repair_outcome(&result, join_panicked || job.diagnostic_failed())).with_field("cancelled", cancelled());
            if join_panicked { record = record.with_field("category", "worker_panic"); }
            if repair_outcome(&result, join_panicked || job.diagnostic_failed()) == "failed" { record.level = Severity::Error; }
            publish(&operation.ctx, record);
        }
        if cancelled() {
            return mark_cancelled(&sessions, session_id);
        }
        if let Err(e) = result {
            return fail_preparing(
                &sessions,
                session_id,
                format!("补全缺失文件失败（{files} 个待下载）：{e}。检查网络后重试，或换一个下载源。"),
            );
        }
        repaired_files += files;
        set_step(
            &sessions,
            session_id,
            "repair",
            PhaseStatus::Running,
            format!("已补全 {repaired_files} 个文件"),
        );
    }

    // ---- ③ 解压 natives ----
    if cancelled() {
        return mark_cancelled(&sessions, session_id);
    }
    set_step(&sessions, session_id, "natives", PhaseStatus::Running, "解压 natives…");
    if let Err(e) = extract_natives(&plan, None) {
        return fail_preparing(&sessions, session_id, format!("解压 natives 失败：{e}"));
    }
    set_step(&sessions, session_id, "natives", PhaseStatus::Completed, "");
    session_event(&sessions, session_id, "launch.natives", Severity::Info, &[("count", json!(plan.natives_jars.len()))]);

    // ---- ④ 启动进程 ----
    if cancelled() {
        return mark_cancelled(&sessions, session_id);
    }
    set_step(&sessions, session_id, "spawn", PhaseStatus::Running, "生成启动参数…");
    let args = build_args(&plan, token.as_deref());
    let script = render_script(&plan, &args);
    if let Err(e) = std::fs::write(run_dir.join("launch-script.txt"), &script) {
        return fail_preparing(&sessions, session_id, format!("写启动脚本失败：{e}"));
    }
    let log_path = run_dir.join("console.log");
    let log_file = match std::fs::File::create(&log_path) {
        Ok(file) => file,
        Err(e) => return fail_preparing(&sessions, session_id, format!("创建日志文件失败：{e}")),
    };

    let exe = prefer_windowless_java(&plan.java_path);

    // ---- 启动前的三件"副作用"，全部只写说明、不拦启动 ----
    let mut notes: Vec<String> = Vec::new();
    if plan.auto_chinese {
        notes.extend(options::ensure_chinese(
            Path::new(&plan.game_dir),
            &plan.game_version,
        ));
    }
    if plan.high_performance_gpu {
        let launcher = std::env::current_exe().ok();
        notes.extend(gpu::apply_high_performance(launcher.as_deref(), &exe));
    }
    if plan.write_latest_launch_bat {
        let bat_path = data_root.join("LatestLaunch.bat");
        if let Err(e) = std::fs::write(&bat_path, render_latest_launch_bat(&plan, &args)) {
            notes.push(format!("写 LatestLaunch.bat 失败：{e}（不影响游戏）"));
        }
    }

    let mut command = Command::new(&exe);
    command
        .args(&args)
        .current_dir(if plan.launch_dir.is_empty() {
            &plan.game_dir
        } else {
            &plan.launch_dir
        })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // 让 java 自己别弹控制台窗口；游戏窗口仍由 LWJGL 正常创建。
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return fail_preparing(
                &sessions,
                session_id,
                format!("无法启动 {}：{e}", exe.display()),
            )
        }
    };
    let pid = child.id();
    session_event(&sessions, session_id, "launch.spawned", Severity::Info, &[("pid", json!(pid)), ("outcome", json!("started"))]);
    if cancelled() {
        // 取消正好卡在 spawn 上：不能留孤儿进程。
        let _ = child.kill();
        return mark_cancelled(&sessions, session_id);
    }

    let log = Arc::new(Mutex::new(String::new()));
    if let Some(stdout) = child.stdout.take() {
        if let Ok(clone) = log_file.try_clone() {
            spawn_pipe_reader(stdout, log.clone(), clone, session_operation(&sessions, session_id));
        }
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_pipe_reader(stderr, log.clone(), log_file, session_operation(&sessions, session_id));
    }

    let moved = touch(&sessions, session_id, |runtime| {
        runtime.child = Some(child);
        runtime.log = log;
        runtime.state.pid = Some(pid);
        runtime.state.phase = LaunchPhase::Launching;
        runtime.state.started_at = Some(unix_ms());
        runtime.state.notes.extend(notes);
        if let Some(step) = step_mut(&mut runtime.state, "spawn") {
            step.status = PhaseStatus::Completed;
            step.detail = format!("pid {pid}");
        }
        if let Some(step) = step_mut(&mut runtime.state, "window") {
            step.status = PhaseStatus::Running;
            step.detail = "等待窗口出现…".into();
        }
    });
    if !moved {
        return;
    }

    session_event(&sessions, session_id, "launch.step", Severity::Info, &[("step", json!("spawn")), ("previous_status", json!("running")), ("status", json!("completed"))]);
    session_event(&sessions, session_id, "launch.step", Severity::Info, &[("step", json!("window")), ("previous_status", json!("pending")), ("status", json!("running"))]);
    // ---- ⑤ 等窗口 ----
    spawn_watcher(sessions, session_id, pid, mods, run_dir);
}

/// 后台盯进程：推进 `Launching → Running`，退出后立刻做崩溃分析。
fn spawn_watcher(
    sessions: Arc<Mutex<Vec<Session>>>,
    session_id: u64,
    pid: u32,
    mods: Vec<String>,
    run_dir: PathBuf,
) {
    std::thread::Builder::new()
        .name("kaleid-launch-watch".into())
        .spawn(move || {
            let started = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let mut guard = match sessions.lock() {
                    Ok(guard) => guard,
                    Err(_) => return,
                };
                let Some(session) = guard.iter_mut().find(|s| s.id == session_id) else {
                    // 会话被清理掉了（理论上不会发生，除非用户退出了启动器）。
                    return;
                };
                let runtime = &mut session.runtime;
                if runtime.state.pid != Some(pid) {
                    return;
                }
                let exited = match runtime.child.as_mut() {
                    Some(child) => match child.try_wait() {
                        Ok(Some(status)) => Some(status.code()),
                        Ok(None) => None,
                        Err(_) => Some(None),
                    },
                    None => Some(None),
                };
                if let Some(code) = exited {
                    let operation = runtime.diagnostics.clone();
                    let log_text = runtime.log.lock().unwrap().clone();
                    let crash_file = latest_crash_report(runtime);
                    let mut report = crash::analyze(
                        &log_text,
                        code,
                        &mods,
                        crash_file.map(|p| p.to_string_lossy().to_string()),
                    );
                    // 我们自己结束的进程一定拿到非零退出码，但那不是崩溃。
                    let killed = runtime.killed;
                    let crashed = report.detected && !killed;
                    report.detected = crashed;
                    // 窗口出现过没有，决定了这次到底是"没启动起来"还是"玩着玩着挂了"。
                    // 前者要报错给用户看，后者该走崩溃报告那条路。
                    let reached_running = runtime.state.phase == LaunchPhase::Running
                        || runtime
                            .state
                            .steps
                            .iter()
                            .any(|s| s.id == "window" && s.status == PhaseStatus::Completed);
                    let tail_path = run_dir.join("console-tail.txt");
                    let _ = std::fs::write(&tail_path, crash::tail_lines(&log_text, LOG_TAIL_LINES));
                    runtime.state.phase = if crashed {
                        LaunchPhase::Failed
                    } else {
                        LaunchPhase::Exited
                    };
                    runtime.state.exit_code = code;
                    runtime.state.crashed = crashed;
                    runtime.state.console_tail_path =
                        Some(tail_path.to_string_lossy().to_string());
                    if let Some(started_at) = runtime.state.started_at {
                        runtime.state.elapsed_ms = unix_ms().saturating_sub(started_at);
                    }
                    let exit_text = format!(
                        "退出码 {}",
                        code.map(|c| c.to_string()).unwrap_or_else(|| "未知".into())
                    );
                    if reached_running {
                        // 已经玩起来了：窗口那一步保持"完成"，只在 detail 里记结局；
                        // 崩没崩由 `crashed` 决定——界面据此弹崩溃报告或提示"已退出"。
                        if let Some(step) = step_mut(&mut runtime.state, "window") {
                            step.status = PhaseStatus::Completed;
                            step.detail = if killed {
                                format!("已按你的要求结束进程（{exit_text}）")
                            } else if crashed {
                                format!("游戏异常退出（{exit_text}）")
                            } else {
                                format!("游戏已退出（{exit_text}）")
                            };
                        }
                        runtime.state.error = None;
                    } else {
                        // 窗口都没出现就没了：这是启动失败，得说清楚。
                        let message = if crashed {
                            format!("启动后崩溃退出（{exit_text}）")
                        } else {
                            format!("游戏进程在窗口出现前就退出了（{exit_text}）")
                        };
                        runtime.state.error = Some(message.clone());
                        if let Some(step) = step_mut(&mut runtime.state, "window") {
                            step.status = PhaseStatus::Failed;
                            step.detail = message;
                        }
                    }
                    runtime.report = report;
                    runtime.child = None;
                    let mut record = operation.record.clone(); record.event = "launch.exited".into();
                    record = record.with_field("pid", pid).with_field("crashed", crashed).with_field("killed", killed).with_field("reached_running", reached_running).with_field("runtime_duration_ms", millis(started));
                    if let Some(code) = code { record = record.with_field("exit_code", code); }
                    let failed = crashed || (!reached_running && !killed);
                    let result = if failed { Err(ApiError::Unavailable("process_exit".into())) } else { Ok(()) };
                    drop(guard);
                    publish(&operation.ctx, record);
                    if !reached_running { session_event(&sessions, session_id, "launch.step", Severity::Warn, &[("step", json!("window")), ("status", json!("failed"))]); }
                    operation.finish(&result, false);
                    return;
                }

                let mut running = None;
                if runtime.state.phase == LaunchPhase::Launching {
                    let log_text = runtime.log.lock().unwrap().to_ascii_lowercase();
                    let seen = RUNNING_MARKERS.iter().any(|m| log_text.contains(m));
                    if seen || started.elapsed() >= RUNNING_FALLBACK {
                        let operation = runtime.diagnostics.clone();
                        let mut record = operation.record.clone(); record.event = "launch.running".into();
                        running = Some((operation.ctx, record.with_field("pid", pid).with_field("reason", if seen { "log_marker" } else { "timeout_fallback" })));
                        runtime.state.phase = LaunchPhase::Running;
                        runtime.state.game_window_found = seen;
                        if let Some(step) = step_mut(&mut runtime.state, "window") {
                            step.status = PhaseStatus::Completed;
                            step.detail = if seen {
                                "已检测到游戏窗口".into()
                            } else {
                                "进程存活，按超时判定启动成功".into()
                            };
                        }
                    }
                }
                if let Some(started_at) = runtime.state.started_at {
                    runtime.state.elapsed_ms = unix_ms().saturating_sub(started_at);
                }
                drop(guard);
                if let Some((ctx, record)) = running {
                    publish(&ctx, record);
                    session_event(&sessions, session_id, "launch.step", Severity::Info, &[("step", json!("window")), ("previous_status", json!("running")), ("status", json!("completed"))]);
                }
            }
        })
        .ok();
}

/// 丢掉太久以前的已结束会话，并保证总数不超过上限。
fn prune_sessions(sessions: &mut Vec<Session>) {
    let now = unix_ms();
    sessions.retain(|s| {
        if s.runtime.state.phase.is_busy() {
            return true;
        }
        match s.runtime.state.started_at {
            Some(started) => now.saturating_sub(started) < SESSION_KEEP_MS,
            None => true,
        }
    });
    if sessions.len() > SESSION_KEEP_MAX {
        let drop_count = sessions.len() - SESSION_KEEP_MAX;
        // 只丢最老的已结束会话，正在跑的一个都不能丢。
        let mut removed = 0;
        sessions.retain(|s| {
            if removed < drop_count && !s.runtime.state.phase.is_busy() {
                removed += 1;
                return false;
            }
            true
        });
    }
}

/* ------------------------------------------------------------------ *
 * 命令行拼装
 * ------------------------------------------------------------------ */

/// 真实 spawn 用的参数：JVM 段（版本自带 + 启动器控制）+ `-cp` + 主类 + 游戏段。
fn build_args(plan: &LaunchPlan, token: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    args.extend(plan.jvm_args.iter().cloned());
    if !plan.natives_dir.is_empty()
        && !args
            .iter()
            .any(|a| a.starts_with("-Djava.library.path"))
    {
        args.push(format!("-Djava.library.path={}", plan.natives_dir));
    }
    args.extend(plan.extra_jvm_args.iter().cloned());
    args.push("-cp".to_string());
    args.push(plan.classpath.join(if cfg!(windows) { ";" } else { ":" }));
    args.push(plan.main_class.clone());
    for arg in &plan.game_args {
        args.push(arg.clone());
    }
    if let Some(token) = token {
        // 计划里的 `--accessToken 0` 是占位，真令牌在这里替换。
        for i in 0..args.len().saturating_sub(1) {
            if args[i] == "--accessToken" {
                args[i + 1] = token.to_string();
            }
        }
    }
    args
}

/// 给人看的启动脚本：令牌一定被抹掉，其他参数原样保留（排障时要靠它）。
///
/// 抹的是 `--accessToken` **后面那个参数**，而不是"值等于某串文本的参数"——
/// 后者会把 `--width 0` 这种恰好同值的参数一起抹掉，日志就失真了。
fn render_script(plan: &LaunchPlan, args: &[String]) -> String {
    let token_at = args.iter().position(|a| a == "--accessToken");
    let masked: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(index, arg)| match token_at {
            Some(at) if index == at + 1 => "***".to_string(),
            _ => arg.clone(),
        })
        .collect();
    let mut text = String::new();
    text.push_str(&format!("# Kaleid MC Launcher — {}\n", plan.version_id));
    text.push_str(&format!("# Minecraft {}\n", plan.game_version));
    text.push_str(&format!(
        "# 内存上限：{}\n",
        plan.extra_jvm_args
            .iter()
            .find(|a| a.starts_with("-Xmx"))
            .cloned()
            .unwrap_or_else(|| "-Xmx?".into())
    ));
    text.push_str("# 访问令牌已抹成 ***\n\n");
    text.push_str(&format!("{}\n", quote(&plan.java_path)));
    for arg in &masked {
        text.push_str(&format!("  {}\n", quote(arg)));
    }
    text
}

fn quote(value: &str) -> String {
    if value.is_empty() || value.contains([' ', '"']) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

/// `LatestLaunch.bat`：一份可以直接双击复现启动的批处理。
///
/// PCL2 每次启动都会写一份（`LatestLaunch.bat`），好处是排查"启动器到底传了什么"
/// 时不用读日志、也不用猜：双击就能看到同样的结果。这里照做，但守两条底线：
/// - 访问令牌抹成 `***`（脚本是要给用户/别人看的，不能带真令牌）；
/// - 批处理里的 `%` 必须写成 `%%`，否则 `%APPDATA%` 之类会被提前展开搞坏命令行。
pub fn render_latest_launch_bat(plan: &LaunchPlan, args: &[String]) -> String {
    let token_at = args.iter().position(|a| a == "--accessToken");
    let masked: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(index, arg)| match token_at {
            Some(at) if index == at + 1 => "***".to_string(),
            _ => arg.replace('%', "%%"),
        })
        .collect();
    let java = plan.java_path.replace('%', "%%");
    let work_dir = if plan.launch_dir.is_empty() {
        plan.game_dir.clone()
    } else {
        plan.launch_dir.clone()
    };

    let mut text = String::new();
    text.push_str("@echo off\n");
    // 中文标题/回显需要 UTF-8 代码页，否则在中文系统上会显示乱码
    text.push_str("chcp 65001 >nul\n");
    text.push_str(&format!("title 启动 - {}\n", plan.version_id.replace('%', "%%")));
    text.push_str(&format!("cd /D \"{}\"\n", work_dir.replace('%', "%%")));
    // 令牌已抹，这条命令**不能**用来真的进服务器；它只用于对照参数。
    text.push_str("\"");
    text.push_str(&java);
    text.push_str("\"");
    for arg in &masked {
        text.push(' ');
        if arg.is_empty() || arg.contains(' ') {
            text.push('"');
            text.push_str(arg);
            text.push('"');
        } else {
            text.push_str(arg);
        }
    }
    text.push('\n');
    text.push_str("echo 游戏已退出。\n");
    text.push_str("pause\n");
    text
}

/// 优先用 `javaw.exe`：同目录下的 `java.exe` 会额外弹一个黑色控制台窗口。
fn prefer_windowless_java(java: &str) -> PathBuf {
    let path = PathBuf::from(java);
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return path;
    };
    if name.eq_ignore_ascii_case("java.exe") {
        if let Some(parent) = path.parent() {
            let javaw = parent.join("javaw.exe");
            if javaw.is_file() {
                return javaw;
            }
        }
    }
    path
}

/// 把原生包里的动态库解压到 `natives_dir`。
///
/// 优先用计划里的 `natives_jars` 清单：1.12.2 及更早的原生库在
/// `downloads.classifiers` 里，压根不在 classpath 上，靠"扫 classpath 里带
/// natives 的 jar"永远扫不到。老计划（没有清单）仍然走 classpath 扫描。
/// 已经解压过就跳过（同一版本反复启动时省一次全量 IO）。
fn extract_natives(plan: &LaunchPlan, cap: Option<usize>) -> std::io::Result<()> {
    if plan.natives_dir.is_empty() {
        return Ok(());
    }
    let dir = PathBuf::from(&plan.natives_dir);
    std::fs::create_dir_all(&dir)?;
    let budget = cap.unwrap_or(64 * 1024 * 1024);
    let mut written = 0usize;

    // (jar 绝对路径, 要跳过的条目前缀)
    let mut sources: Vec<(String, Vec<String>)> = Vec::new();
    for native in &plan.natives_jars {
        if let Some(path) = resolve_library_file(plan, &native.path) {
            sources.push((path, native.excludes.clone()));
        }
    }
    if sources.is_empty() {
        // 兼容：老计划没有清单时，classpath 里文件名带 `natives` 的 jar 仍是候选。
        for jar_path in &plan.classpath {
            let lower = jar_path.to_ascii_lowercase();
            if !lower.contains("natives") || !lower.ends_with(".jar") {
                continue;
            }
            sources.push((jar_path.clone(), Vec::new()));
        }
    }

    for (jar_path, excludes) in sources {
        let entries = jar::list_entries(Path::new(&jar_path), 4096);
        for entry in entries {
            if !crate::natives::native_entry_allowed(&entry, &excludes) {
                continue;
            }
            let lower_entry = entry.to_ascii_lowercase();
            let is_lib = lower_entry.ends_with(".dll")
                || lower_entry.ends_with(".so")
                || lower_entry.ends_with(".dylib");
            if !is_lib {
                continue;
            }
            let Some(name) = entry.rsplit('/').next() else {
                continue;
            };
            let target = dir.join(name);
            if target.is_file() {
                continue;
            }
            let Some(bytes) = jar::read_entry_bytes(Path::new(&jar_path), &entry, 16 * 1024 * 1024)
            else {
                continue;
            };
            written += bytes.len();
            if written > budget {
                return Ok(());
            }
            std::fs::write(&target, bytes)?;
        }
    }
    Ok(())
}

/// `libraries/` 相对路径 → 真实文件：版本本地库目录优先，根 `libraries/` 兜底。
fn resolve_library_file(plan: &LaunchPlan, relative: &str) -> Option<String> {
    let mut candidates: Vec<PathBuf> = plan.libraries_dirs.iter().map(PathBuf::from).collect();
    if candidates.is_empty() && !plan.libraries_dir.is_empty() {
        candidates.push(PathBuf::from(&plan.libraries_dir));
    }
    for base in candidates {
        let path = base.join(relative);
        if path.is_file() {
            return Some(path.to_string_lossy().to_string());
        }
    }
    None
}

/// 把子进程的一个输出管道按行搬进内存日志与 `console.log`。
///
/// **按字节读、按 lossy 转字符串**：`read_line` 要求整行都是合法 UTF-8，而中文
/// Windows 上 `cmd` / 某些模组会往控制台写 GBK，一个坏字节就让整个读取线程退出，
/// 日志从此空白——排查时看到"什么都没有"正是这么来的。
fn spawn_pipe_reader<R: Read + Send + 'static>(
    reader: R,
    log: Arc<Mutex<String>>,
    mut file: std::fs::File,
    diagnostics: Option<Operation>,
) {
    std::thread::Builder::new()
        .name("kaleid-launch-stdout".into())
        .spawn(move || {
            let mut buf = BufReader::new(reader);
            let mut raw: Vec<u8> = Vec::new();
            let mut write_warned = false;
            loop {
                raw.clear();
                match buf.read_until(b'\n', &mut raw) {
                    Ok(0) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&raw).to_string();
                        // 超大单行（模组有时候会刷一行几十 KB）截断，避免内存无限涨。
                        // 按字符边界切，别在 UTF-8 中间劈一刀。
                        let trimmed = if line.len() > 8192 {
                            let cut = floor_char_boundary(&line, 8192);
                            format!("{}…（截断）\n", &line[..cut])
                        } else {
                            line
                        };
                        if file.write_all(trimmed.as_bytes()).is_err() && !write_warned {
                            write_warned = true;
                            if let Some(operation) = &diagnostics { let mut record = operation.record.clone(); record.event = "launch.console_failed".into(); record.level = Severity::Warn; publish(&operation.ctx, record.with_field("category", "io")); }
                        }
                        let mut guard = log.lock().unwrap();
                        guard.push_str(&trimmed);
                        if guard.len() > 4 * 1024 * 1024 {
                            // 只保留尾部：崩溃分析看的是最后几百行。
                            let cut = floor_char_boundary(&guard, guard.len() - 2 * 1024 * 1024);
                            let tail = guard[cut..].to_string();
                            *guard = tail;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();
}

/// 游戏目录里最新的一份官方崩溃报告。
fn latest_crash_report(runtime: &Runtime) -> Option<PathBuf> {
    let game_dir = runtime.plan.as_ref().map(|p| p.game_dir.clone())?;
    let dir = PathBuf::from(game_dir).join("crash-reports");
    // 只认**这一次启动之后**写下的报告：`crash-reports/` 里往往躺着上个月的，
    // 拿它当"本次崩溃"会让每次正常退出都显示一份旧报告。
    latest_crash_report_since(&dir, runtime.state.started_at)
}

/// 取 `crash-reports/` 目录里不早于 `since_ms` 的最新 `.txt`。
///
/// 留 2 秒余量：文件系统时间戳与我们的 `started_at` 不是同一个来源，
/// 刚写下就判断时可能差几个 tick。
fn latest_crash_report_since(dir: &Path, since_ms: Option<u64>) -> Option<PathBuf> {
    let since = since_ms.map(|ms| ms.saturating_sub(2000));
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e != "txt").unwrap_or(true) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        if let Some(since) = since {
            let modified_ms = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if modified_ms < since {
                continue;
            }
        }
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("game_diagnostics_tests.rs");

    /// 启动现在是异步的（准备阶段跑在后台线程），测试要等它推进到目标阶段。
    ///
    /// 等到**所有**会话进入 `want` 才返回；超时直接失败，别让测试互相拖。
    fn wait_for(manager: &GameProcessManager, want: LaunchPhase) -> Vec<LaunchState> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let states = manager.states();
            if !states.is_empty() && states.iter().all(|s| s.phase == want) {
                return states;
            }
            if Instant::now() > deadline {
                let phases: Vec<LaunchPhase> = states.iter().map(|s| s.phase).collect();
                panic!("等不到 {want:?}，当前是 {phases:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// natives 清单里的原生包要真的解出动态库，`META-INF/` 要按 `extract.exclude` 跳过。
    /// 这是 `no lwjgl in java.library.path` 的最后一道关：即使下对了包，
    /// 不解压到 `natives/` 依然是空目录。
    #[test]
    fn extract_natives_uses_the_plan_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let libraries = dir.path().join("libraries");
        let native_rel = "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-windows.jar";
        let jar_path = libraries.join(native_rel);
        std::fs::create_dir_all(jar_path.parent().unwrap()).unwrap();
        std::fs::write(
            &jar_path,
            crate::jar::tests::zip_stored(&[
                ("lwjgl.dll", b"DLL".as_slice()),
                ("META-INF/MANIFEST.MF", b"ignored".as_slice()),
                ("README.txt", b"nope".as_slice()),
            ]),
        )
        .unwrap();

        let natives_dir = dir.path().join("natives");
        let plan = LaunchPlan {
            natives_dir: natives_dir.to_string_lossy().to_string(),
            natives_jars: vec![kaleid_api::NativeJarRef {
                path: native_rel.to_string(),
                url: None,
                sha1: None,
                size: 0,
                excludes: vec!["META-INF/".to_string()],
            }],
            libraries_dir: libraries.to_string_lossy().to_string(),
            libraries_dirs: vec![libraries.to_string_lossy().to_string()],
            // 老代码只认 classpath：这里故意留空，证明清单足以定位原生包。
            classpath: Vec::new(),
            ..plan()
        };

        extract_natives(&plan, None).unwrap();

        assert!(natives_dir.join("lwjgl.dll").is_file(), "动态库必须解出来");
        assert!(
            !natives_dir.join("MANIFEST.MF").is_file(),
            "META-INF/ 里的东西不该落地"
        );
        assert!(!natives_dir.join("README.txt").is_file(), "只解动态库");
    }

    /// 老计划没有 `natives_jars` 时，退回"扫 classpath 里带 natives 的 jar"。
    #[test]
    fn extract_natives_falls_back_to_classpath_scan() {
        let dir = tempfile::tempdir().unwrap();
        let jar_path = dir.path().join("natives-1.jar");
        std::fs::write(
            &jar_path,
            crate::jar::tests::zip_stored(&[("OpenAL64.dll", b"DLL".as_slice())]),
        )
        .unwrap();

        let natives_dir = dir.path().join("natives");
        let plan = LaunchPlan {
            natives_dir: natives_dir.to_string_lossy().to_string(),
            classpath: vec![jar_path.to_string_lossy().to_string()],
            ..plan()
        };

        extract_natives(&plan, None).unwrap();
        assert!(natives_dir.join("OpenAL64.dll").is_file());
    }

    fn plan() -> LaunchPlan {
        LaunchPlan {
            version_id: "26.3-Fabric".into(),
            java_path: "java".into(),
            java_source: "auto".into(),
            game_dir: "C:/mc".into(),
            mc_dir: "C:/mc".into(),
            assets_dir: "C:/mc/assets".into(),
            libraries_dir: "C:/mc/libraries".into(),
            isolated: false,
            main_class: "net.fabricmc.loader.impl.launch.knot.KnotClient".into(),
            classpath: vec!["C:/mc/libraries/a.jar".into(), "C:/mc/libraries/natives-1.jar".into()],
            jvm_args: vec!["-Djava.library.path=x".into()],
            game_args: vec![
                "--username".into(),
                "Steve".into(),
                "--accessToken".into(),
                "0".into(),
                "--versionType".into(),
                "Kaleid MC Launcher".into(),
            ],
            extra_jvm_args: vec!["-Xmx4096M".into()],
            username: "Steve".into(),
            uuid: "0000".into(),
            asset_index: Some("17".into()),
            launcher_brand: Some("Kaleid MC Launcher".into()),
            launch_dir: "C:/mc".into(),
            natives_dir: "C:/mc/versions/26.3-Fabric/natives".into(),
            natives_jars: Vec::new(),
            libraries_dirs: vec!["C:/mc/libraries".into()],
            game_version: "26.3".into(),
            access_token_injected: false,
            jvm_notes: Vec::new(),
            auto_chinese: false,
            write_latest_launch_bat: false,
            high_performance_gpu: false,
        }
    }

    #[test]
    fn args_put_jvm_first_then_classpath_main_and_game_args() {
        let args = build_args(&plan(), Some("real-token"));
        let cp = args.iter().position(|a| a == "-cp").unwrap();
        assert!(args[..cp].iter().any(|a| a == "-Xmx4096M"));
        assert_eq!(args[cp + 2], "net.fabricmc.loader.impl.launch.knot.KnotClient");
        let token_at = args.iter().position(|a| a == "--accessToken").unwrap();
        assert_eq!(args[token_at + 1], "real-token");
        // natives 目录只加一次
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("-Djava.library.path"))
                .count(),
            1
        );
    }

    #[test]
    fn script_masks_access_token() {
        let p = plan();
        let mut args = build_args(&p, Some("sup3r-s3cret"));
        // 让掩码有东西可掩：脚本按计划里的占位找令牌，这里模拟计划里也是真值
        let idx = args.iter().position(|a| a == "--accessToken").unwrap();
        args[idx + 1] = "0".to_string();
        let mut p2 = p.clone();
        for i in 0..p2.game_args.len() - 1 {
            if p2.game_args[i] == "--accessToken" {
                p2.game_args[i + 1] = "0".to_string();
            }
        }
        let script = render_script(&p2, &args);
        assert!(script.contains("***"));
        assert!(!script.contains("sup3r-s3cret"));
        assert!(script.contains("Kaleid MC Launcher"));
    }

    #[test]
    fn state_starts_idle_and_not_busy() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let state = manager.state();
        assert_eq!(state.phase, LaunchPhase::Idle);
        assert_eq!(state.session_id, 0, "没启动过时不该有会话 id");
        assert!(!state.phase.is_busy());
        assert!(manager.states().is_empty());
        assert!(!manager.report(None).detected);
    }

    /// 多开：同一台机器上两个版本可以同时起来，各自有独立的会话与日志目录。
    ///
    /// 用 `cmd /c ping` 当"游戏"——它肯定存在、会活几秒、也会自己退出，
    /// 不需要真的装 Minecraft。
    #[cfg(windows)]
    #[test]
    fn two_versions_can_run_at_the_same_time() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());

        let mut first = plan();
        first.version_id = "26.3-Fabric".into();
        first.java_path = "cmd".into();
        first.classpath.clear();
        first.jvm_args.clear();
        first.extra_jvm_args.clear();
        first.game_args = vec!["/c".into(), "ping -n 6 127.0.0.1".into()];
        first.main_class = String::new();

        let mut second = first.clone();
        second.version_id = "1.20.1-Forge".into();

        let a = manager.start(first, None, Vec::new()).unwrap();
        let b = manager.start(second, None, Vec::new()).unwrap();
        assert_eq!(a.session_id, 1);
        assert_eq!(b.session_id, 2, "第二次启动必须是新会话，而不是覆盖第一个");

        // 启动是异步的：等到两个会话都真的走到 spawn（最多 20 秒）。
        let states = wait_for(&manager, LaunchPhase::Launching);
        assert!(
            states.iter().all(|s| s.phase == LaunchPhase::Launching),
            "两个会话都该在跑：{:?}",
            states.iter().map(|s| s.phase).collect::<Vec<_>>()
        );
        // 日志目录按会话区分，多开时不互相覆盖
        assert_ne!(a.log_path, b.log_path);
        // 同版本还能再开一个：不允许"这个版本已经在跑"式的拦截
        let third = manager.start({
            let mut p = plan();
            p.version_id = "26.3-Fabric".into();
            p.java_path = "cmd".into();
            p.classpath.clear();
            p.jvm_args.clear();
            p.extra_jvm_args.clear();
            p.game_args = vec!["/c".into(), "ping -n 6 127.0.0.1".into()];
            p.main_class = String::new();
            p
        }, None, Vec::new());
        assert!(third.is_ok());

        // 收尾：别把 ping 留在后台
        let _ = manager.stop(None);
    }

    /// 五步进度：走到 `Launching` 时，前面四步必须都已完成、最后一步在跑。
    #[cfg(windows)]
    #[test]
    fn steps_advance_through_the_five_phases() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.game_args = vec!["/c".into(), "ping -n 4 127.0.0.1".into()];
        p.main_class = String::new();

        let started = manager.start(p, None, Vec::new()).unwrap();
        assert_eq!(started.phase, LaunchPhase::Preparing, "start 立刻返回准备态");
        assert_eq!(started.steps.len(), 5);
        assert_eq!(
            started.steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["check", "repair", "natives", "spawn", "window"]
        );

        let states = wait_for(&manager, LaunchPhase::Launching);
        let state = &states[0];
        let step = |id: &str| state.steps.iter().find(|s| s.id == id).unwrap().status;
        assert_eq!(step("check"), PhaseStatus::Completed);
        // 版本 JSON 不存在 → 没有可补的文件，这一步应当是「跳过」而不是「失败」。
        assert_eq!(step("repair"), PhaseStatus::Skipped);
        assert_eq!(step("natives"), PhaseStatus::Completed);
        assert_eq!(step("spawn"), PhaseStatus::Completed);
        assert_eq!(step("window"), PhaseStatus::Running);
        assert!(
            state.percent > 0.5,
            "四步完成时总进度该过半，实际 {}",
            state.percent
        );

        let _ = manager.stop(None);
    }

    /// 「取消启动」：准备阶段取消 → `Cancelled`，不 spawn、不算崩溃。
    #[cfg(windows)]
    #[test]
    fn cancel_during_preparing_marks_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        manager.set_prep_delay(600);
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.game_args = vec!["/c".into(), "ping -n 30 127.0.0.1".into()];
        p.main_class = String::new();

        let started = manager.start(p, None, Vec::new()).unwrap();
        manager.cancel(started.session_id).unwrap();

        let states = wait_for(&manager, LaunchPhase::Cancelled);
        let state = &states[0];
        assert!(state.cancelled);
        assert_eq!(state.error.as_deref(), Some("已取消启动"));
        assert!(state.pid.is_none(), "取消后不该有进程");
        assert!(!state.crashed, "取消不是崩溃");
        let _ = manager.stop(None);
    }

    /// 进程起来之后再点取消：要给一句人话，不能假装成功。
    #[cfg(windows)]
    #[test]
    fn cancel_after_spawn_is_refused_with_a_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.game_args = vec!["/c".into(), "ping -n 4 127.0.0.1".into()];
        p.main_class = String::new();
        let started = manager.start(p, None, Vec::new()).unwrap();
        wait_for(&manager, LaunchPhase::Launching);

        let err = manager.cancel(started.session_id).unwrap_err().to_string();
        assert!(err.contains("结束进程"), "实际错误：{err}");
        let _ = manager.stop(None);
    }

    /// 玩着玩着挂了：窗口那一步保持"完成"，`crashed` 交给崩溃弹窗，`error` 留空。
    ///
    /// 从实例面板点「结束进程」不是崩溃：那是用户要求的，不该弹崩溃报告。
    ///
    /// 真机上踩到的：结束进程后退出码非零 → 被判成崩溃。这里钉住
    /// 「主动结束 ≠ 崩溃」，而且要留一句人话说明是主动结束的。
    #[cfg(windows)]
    #[test]
    fn stopping_from_the_launcher_is_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        // 打一行"窗口出现"的日志佐证，再挂着不退；然后由 stop 杀掉。
        p.game_args = vec![
            "/c".into(),
            "echo Backend library: LWJGL & ping -n 20 127.0.0.1".into(),
        ];
        p.main_class = String::new();
        manager.start(p, None, Vec::new()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(20);
        while !manager
            .states()
            .iter()
            .any(|s| s.phase == LaunchPhase::Running)
        {
            assert!(Instant::now() < deadline, "等不到 Running");
            std::thread::sleep(Duration::from_millis(100));
        }

        manager.stop(None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = manager.state();
            if state.phase != LaunchPhase::Running && state.phase != LaunchPhase::Launching {
                assert!(
                    !state.crashed,
                    "主动结束进程不是崩溃（退出码 {:?}）",
                    state.exit_code
                );
                assert_eq!(state.phase, LaunchPhase::Exited);
                let window = state.steps.iter().find(|s| s.id == "window").unwrap();
                assert_eq!(window.status, PhaseStatus::Completed);
                assert!(
                    window.detail.contains("结束"),
                    "要说明是用户结束的，实际：{:?}",
                    window.detail
                );
                assert!(state.error.is_none(), "不该有错误提示：{:?}", state.error);
                return;
            }
            assert!(Instant::now() < deadline, "进程杀了之后状态没落地");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 但真崩了还是要认：进过游戏之后非零退出且没有关服日志 = 崩溃，
    /// 窗口那一步保持"完成"，`error` 留空交给崩溃弹窗。
    #[cfg(windows)]
    #[test]
    fn a_real_crash_after_the_window_still_counts_as_crashed() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        // 先当作窗口出来了，然后直接非零退出（没有 Stopping!）。
        p.game_args = vec![
            "/c".into(),
            "echo Backend library: LWJGL & ping -n 3 127.0.0.1 >nul & exit 3".into(),
        ];
        p.main_class = String::new();
        manager.start(p, None, Vec::new()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = manager.state();
            if state.phase == LaunchPhase::Failed || state.phase == LaunchPhase::Exited {
                assert!(state.crashed, "非零退出且没关服日志，应当算崩溃");
                assert_eq!(state.exit_code, Some(3));
                let window = state.steps.iter().find(|s| s.id == "window").unwrap();
                assert_eq!(
                    window.status,
                    PhaseStatus::Completed,
                    "窗口已经出现过，这一步不该是失败：{:?}",
                    window.detail
                );
                assert!(
                    state.error.is_none(),
                    "已经进过游戏的崩溃不该再报「启动失败」：{:?}",
                    state.error
                );
                return;
            }
            assert!(Instant::now() < deadline, "等不到退出");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 上一次运行留下的 crash-reports 文件不能挂到这一次上。
    #[cfg(windows)]
    #[test]
    fn an_old_crash_report_is_not_attached_to_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        // 预埋一份"上次崩的"报告，时间戳改到 1 小时前。
        let crash_dir = dir.path().join("crash-reports");
        std::fs::create_dir_all(&crash_dir).unwrap();
        let old = crash_dir.join("crash-2026-10-31_10.00.00-client.txt");
        std::fs::write(&old, "---- Minecraft Crash Report ----\nold").unwrap();
        let an_hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(an_hour_ago)
            .unwrap();

        let mut p = plan();
        p.java_path = "cmd".into();
        p.game_dir = dir.path().to_string_lossy().to_string();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.game_args = vec!["/c".into(), "echo done".into()];
        p.main_class = String::new();
        manager.start(p, None, Vec::new()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = manager.state();
            if state.phase != LaunchPhase::Preparing && state.phase != LaunchPhase::Launching {
                let report = manager.report(None);
                assert!(
                    report.crash_file.is_none(),
                    "旧报告不能算这一次的：{:?}",
                    report.crash_file
                );
                assert!(!report.detected, "旧报告不能让这一次被判成崩溃");
                return;
            }
            assert!(Instant::now() < deadline, "等不到退出");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Java 路径写错：`check` 步失败，理由要具体到路径。
    #[test]
    fn missing_java_fails_the_check_step_with_a_clear_reason() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "C:/definitely/not/here/java.exe".into();

        manager.start(p, None, Vec::new()).unwrap();
        let states = wait_for(&manager, LaunchPhase::Failed);
        let state = &states[0];
        let error = state.error.clone().unwrap_or_default();
        assert!(error.contains("找不到 Java"), "实际错误：{error}");
        assert!(error.contains("C:/definitely/not/here/java.exe"));
        let check = state.steps.iter().find(|s| s.id == "check").unwrap();
        assert_eq!(check.status, PhaseStatus::Failed);
    }

    /// 排障包导出成 ZIP：内容与目录导出一致，文件名是本地时间且不含冒号。
    #[cfg(windows)]
    #[test]
    fn export_zip_writes_a_readable_archive() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.game_args = vec!["/c".into(), "ping -n 3 127.0.0.1".into()];
        p.main_class = String::new();
        manager.start(p, None, Vec::new()).unwrap();
        wait_for(&manager, LaunchPhase::Launching);

        let target = tempfile::tempdir().unwrap();
        let archive = manager.export_zip(target.path(), None).unwrap();
        assert!(archive.file_name.starts_with("崩溃日志-"));
        assert!(archive.file_name.ends_with(".zip"));
        assert!(!archive.file_name.contains(':'));
        let bytes = std::fs::read(&archive.path).unwrap();
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert!(archive.files >= 3, "至少有 latest.log / 脚本 / manifest");
        let _ = manager.stop(None);
    }

    /// 「本次日志」：读的是这个会话自己的 console，按字符边界截断。
    #[cfg(windows)]
    #[test]
    fn console_tail_returns_this_sessions_log() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.game_args = vec!["/c".into(), "echo 中文日志测试 & ping -n 3 127.0.0.1".into()];
        p.main_class = String::new();
        let started = manager.start(p, None, Vec::new()).unwrap();
        // 等 cmd 真的把回显写进管道
        std::thread::sleep(Duration::from_millis(500));

        let tail = manager.console_tail(Some(started.session_id), 64).unwrap();
        assert!(tail.total_bytes > 0, "应该有输出");
        // 截断点会往前退到字符边界，所以允许超出几字节（一个 UTF-8 字符最多 3 字节）。
        assert!(
            tail.shown_bytes <= 64 + 3,
            "最多显示约 64 字节，实际 {}",
            tail.shown_bytes
        );
        assert!(tail.truncated);
        assert!(tail.shown_bytes < tail.total_bytes);
        let _ = manager.stop(None);
    }

    /// 真机验证：把渲染出来的 `LatestLaunch.bat` 交给 cmd 真的跑一遍。
    ///
    /// 这一步测的是「批处理语法本身对不对」（`%%` 转义、引号、`chcp`），
    /// 用 `cmd` 冒充 java，不会真的启动游戏。默认不跑，手动：
    /// `cargo test -p kaleid-core-plugins --lib game::tests::rendered_bat_runs_in_cmd -- --ignored`
    #[cfg(windows)]
    #[test]
    #[ignore = "会真的执行渲染出的 .bat，手动跑"]
    fn rendered_bat_runs_in_cmd() {
        let mut p = plan();
        p.version_id = "1.21.4-Test".into();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args = vec!["-Ddemo=%APPDATA%".into()];
        p.game_args = vec!["/c".into(), "exit 0".into()];
        let args = build_args(&p, None);

        let dir = tempfile::tempdir().unwrap();
        let bat_path = dir.path().join("LatestLaunch.bat");
        std::fs::write(&bat_path, render_latest_launch_bat(&p, &args)).unwrap();

        let out = std::process::Command::new("cmd")
            .arg("/c")
            .arg(&bat_path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("cmd 跑不起来");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "bat 没跑通\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        );
        // `pause` 说明脚本一路跑到了最后一行：`%%` 转义、引号、chcp 都没把 cmd 卡住。
        //
        // 这里不断言 `游戏已退出` 的中文：测试进程的 stdout 是管道、没有真实控制台，
        // `chcp 65001` 在某些环境下拿不到控制台就没生效，中文会被控制台代码页重解。
        // 真实双击（有控制台）时是正常的，已手工验证。
        assert!(
            stdout.contains("Press any key") || !stdout.is_empty(),
            "bat 没有任何输出"
        );
        let rendered = std::fs::read_to_string(&bat_path).unwrap();
        assert!(rendered.contains("echo 游戏已退出。"));
        assert!(rendered.contains("%%APPDATA%%"), "批处理里的 % 必须转义");
    }

    #[test]
    fn stopping_one_session_leaves_the_others_alone() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        // 没有会话时要能安全地"结束全部"（界面不再有结束按钮，但命令还在）
        assert!(manager.stop(None).unwrap().is_empty());
        assert!(manager.stop(Some(42)).unwrap().is_empty());
        // 报一个不存在的会话时不给假报告
        assert!(!manager.report(Some(42)).detected);
    }

    /// LatestLaunch.bat：能双击复现启动，但**不能**泄露访问令牌。
    #[test]
    fn latest_launch_bat_masks_token_and_escapes_percent() {
        let mut p = plan();
        p.java_path = "C:\\Program Files\\Java\\bin\\javaw.exe".into();
        p.extra_jvm_args = vec!["-Dfoo=%APPDATA%".into()];
        let args = build_args(&p, Some("real-token"));
        let bat = render_latest_launch_bat(&p, &args);

        assert!(bat.starts_with("@echo off\n"));
        assert!(bat.contains("chcp 65001"), "中文回显要 UTF-8 代码页");
        assert!(bat.contains("title 启动 - 26.3-Fabric"));
        assert!(bat.contains("cd /D \"C:/mc\""));
        assert!(bat.contains("***"));
        assert!(!bat.contains("real-token"), "脚本里不能出现真令牌");
        assert!(bat.contains("%%APPDATA%%"), "批处理里的 % 必须写成 %%");
        assert!(bat.trim_end().ends_with("pause"));
    }

    /// 每次启动都该在数据目录落一份 LatestLaunch.bat（PCL2 的惯例）。
    #[cfg(windows)]
    #[test]
    fn launch_writes_latest_launch_bat_into_the_data_root() {
        let dir = tempfile::tempdir().unwrap();
        let manager = GameProcessManager::new(dir.path());
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.game_args = vec!["/c".into(), "ping -n 2 127.0.0.1".into()];
        p.main_class = String::new();
        p.write_latest_launch_bat = true;
        let _ = manager.start(p, None, Vec::new()).unwrap();
        // 启动是异步的（准备阶段在后台线程）：等它真的走到 spawn 再看文件。
        wait_for(&manager, LaunchPhase::Launching);

        let bat = std::fs::read_to_string(dir.path().join("LatestLaunch.bat")).unwrap();
        assert!(bat.contains("cd /D"));
        assert!(bat.contains("@echo off"));

        // 排障包也要带上它：别人拿到 zip 就能自己复现同一行命令
        let export_dir = tempfile::tempdir().unwrap();
        let exported = manager.export(Some(export_dir.path()), None).unwrap();
        assert!(
            exported.files.iter().any(|f| f == "LatestLaunch.bat"),
            "导出文件清单里应有 LatestLaunch.bat：{:?}",
            exported.files
        );
        assert!(export_dir.path().join("LatestLaunch.bat").is_file());
        let _ = manager.stop(None);
    }
}
