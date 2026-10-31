//! 共享下载线程池与下载任务。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use sha1::{Digest, Sha1};

use kaleid_api::{
    ApiError, ApiResult, DownloadJob, DownloadPhase, DownloadSnapshot, DownloadStatus, LoaderKind,
    PhaseStatus, SettingsServiceApi,
};

use crate::settings::Settings;

type Task = Box<dyn FnOnce() + Send + 'static>;

struct Executor {
    sender: Sender<Option<Task>>,
    receiver: Receiver<Option<Task>>,
    desired: Arc<AtomicUsize>,
    worker_count: Arc<AtomicUsize>,
    shutdown: Arc<AtomicBool>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Executor {
    fn new(initial: usize) -> Self {
        let (sender, receiver) = unbounded::<Option<Task>>();
        let desired = Arc::new(AtomicUsize::new(initial.max(1)));
        let worker_count = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let workers = Mutex::new(Vec::new());
        let executor = Executor {
            sender,
            receiver: receiver.clone(),
            desired: desired.clone(),
            worker_count: worker_count.clone(),
            shutdown: shutdown.clone(),
            workers,
        };
        executor.spawn_to(initial.max(1), receiver);
        executor
    }

    fn spawn_to(&self, target: usize, receiver: Receiver<Option<Task>>) {
        let mut workers = self.workers.lock().unwrap();
        while self.worker_count.load(Ordering::SeqCst) < target {
            let desired = self.desired.clone();
            let count = self.worker_count.clone();
            let shutdown = self.shutdown.clone();
            let rx = receiver.clone();
            count.fetch_add(1, Ordering::SeqCst);
            let handle = thread::Builder::new()
                .name("kaleid-download".into())
                .stack_size(512 * 1024)
                .spawn(move || loop {
                    if shutdown.load(Ordering::SeqCst) {
                        count.fetch_sub(1, Ordering::SeqCst);
                        break;
                    }
                    match rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(Some(task)) => {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
                        }
                        Ok(None) => {
                            count.fetch_sub(1, Ordering::SeqCst);
                            break;
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            if count.load(Ordering::SeqCst) > desired.load(Ordering::SeqCst) {
                                count.fetch_sub(1, Ordering::SeqCst);
                                break;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            count.fetch_sub(1, Ordering::SeqCst);
                            break;
                        }
                    }
                })
                .expect("无法创建下载线程");
            workers.push(handle);
        }
    }

    fn set_desired(&self, n: usize) {
        let n = n.clamp(1, 256);
        self.desired.store(n, Ordering::SeqCst);
        if self.worker_count.load(Ordering::SeqCst) < n {
            self.spawn_to(n, self.receiver.clone());
        }
    }

    fn submit(&self, task: Task) {
        let _ = self.sender.send(Some(task));
    }

    fn concurrency(&self) -> usize {
        self.desired.load(Ordering::SeqCst)
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let n = self.worker_count.load(Ordering::SeqCst);
        for _ in 0..n {
            let _ = self.sender.send(None);
        }
        if let Ok(mut workers) = self.workers.lock() {
            for w in workers.drain(..) {
                let _ = w.join();
            }
        }
    }
}

struct PhaseState {
    id: String,
    label: String,
    status: PhaseStatus,
    done_bytes: u64,
    total_bytes: u64,
    files_done: usize,
    files_total: usize,
}

struct JobState {
    status: DownloadStatus,
    current: String,
    done_bytes: u64,
    total_bytes: u64,
    files_done: usize,
    files_total: usize,
    error: Option<String>,
    source: String,
    phases: Vec<PhaseState>,
    window_start: Instant,
    window_bytes: u64,
    slow_windows: u8,
}

pub struct Job {
    pub id: String,
    pub title: String,
    pub version_name: String,
    pub root_id: String,
    pub game_version: String,
    pub loader_kind: LoaderKind,
    pub loader_version: Option<String>,
    source_policy: String,
    state: Mutex<JobState>,
    cancel: AtomicBool,
    created_at: u64,
}

impl Job {
    pub fn new(
        id: String,
        title: String,
        version_name: String,
        root_id: String,
        game_version: String,
        loader_kind: LoaderKind,
        loader_version: Option<String>,
        source_policy: String,
    ) -> Arc<Self> {
        // 镜像优先 / 仅镜像的任务一上来就走镜像，队列里的来源标签得如实显示。
        let initial_source = match source_policy.as_str() {
            kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST | kaleid_api::DOWNLOAD_SOURCE_MIRROR_ONLY => {
                "bmclapi"
            }
            _ => "mojang",
        };
        Self::with_initial_source(
            id,
            title,
            version_name,
            root_id,
            game_version,
            loader_kind,
            loader_version,
            source_policy,
            initial_source,
        )
    }

    /// 和 [`Job::new`] 一样，但允许指定**初始来源标签**。
    ///
    /// 模组下载的镜像叫 MCIM 而不是 BMCLAPI，队列里显示 `bmclapi` 会误导用户，
    /// 所以模组任务用 `mcim`；实际从哪个 URL 取仍然由 `DownloadItem` 决定。
    #[allow(clippy::too_many_arguments)]
    pub fn with_initial_source(
        id: String,
        title: String,
        version_name: String,
        root_id: String,
        game_version: String,
        loader_kind: LoaderKind,
        loader_version: Option<String>,
        source_policy: String,
        initial_source: &str,
    ) -> Arc<Self> {
        let mut phases = Vec::new();
        for (id, label) in [
            ("prepare", "解析版本"),
            ("client", "客户端"),
            ("libraries", "依赖库"),
            ("assets", "资源文件"),
            ("loader", "加载器"),
            ("fabric-api", "Fabric API"),
            ("installer", "安装器"),
            ("mods", "模组"),
        ] {
            phases.push(PhaseState {
                id: id.to_string(),
                label: label.to_string(),
                status: PhaseStatus::Pending,
                done_bytes: 0,
                total_bytes: 0,
                files_done: 0,
                files_total: 0,
            });
        }
        Arc::new(Job {
            id,
            title,
            version_name,
            root_id,
            game_version,
            loader_kind,
            loader_version,
            source_policy,
            state: Mutex::new(JobState {
                status: DownloadStatus::Resolving,
                current: "解析版本信息…".into(),
                done_bytes: 0,
                total_bytes: 0,
                files_done: 0,
                files_total: 0,
                error: None,
                source: initial_source.into(),
                phases,
                window_start: Instant::now(),
                window_bytes: 0,
                slow_windows: 0,
            }),
            cancel: AtomicBool::new(false),
            created_at: crate::remote::now_secs(),
        })
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let mut s = self.state.lock().unwrap();
        if matches!(
            s.status,
            DownloadStatus::Resolving | DownloadStatus::Queued | DownloadStatus::Running
        ) {
            s.status = DownloadStatus::Cancelled;
            s.current = "已取消".into();
        }
    }

    pub fn source(&self) -> String {
        self.state.lock().unwrap().source.clone()
    }

    /// 用户设置的下载源策略（official-first / mirror-first / official-only / mirror-only），与当前实际源分开。
    pub fn policy(&self) -> &str {
        &self.source_policy
    }

    pub fn set_status(&self, status: DownloadStatus, current: impl Into<String>) {
        let mut s = self.state.lock().unwrap();
        s.status = status;
        s.current = current.into();
    }

    pub fn set_current(&self, current: impl Into<String>) {
        self.state.lock().unwrap().current = current.into();
    }

    pub fn fail(&self, error: impl Into<String>) {
        let mut s = self.state.lock().unwrap();
        s.status = DownloadStatus::Failed;
        s.error = Some(error.into());
        s.current = "下载失败".into();
        for p in &mut s.phases {
            if p.status == PhaseStatus::Running {
                p.status = PhaseStatus::Failed;
            }
        }
    }

    pub fn complete(&self) {
        let mut s = self.state.lock().unwrap();
        if s.status == DownloadStatus::Cancelled {
            return;
        }
        s.status = DownloadStatus::Completed;
        s.current = "安装完成".into();
        s.error = None;
        for p in &mut s.phases {
            match p.status {
                PhaseStatus::Running => p.status = PhaseStatus::Completed,
                PhaseStatus::Pending => p.status = PhaseStatus::Skipped,
                _ => {}
            }
        }
        if s.total_bytes == 0 {
            s.done_bytes = 0;
            s.total_bytes = 0;
        } else {
            s.done_bytes = s.total_bytes;
        }
        s.files_done = s.files_total;
    }

    pub fn add_total(&self, phase: &str, bytes: u64, files: usize) {
        let mut s = self.state.lock().unwrap();
        s.total_bytes = s.total_bytes.saturating_add(bytes);
        s.files_total = s.files_total.saturating_add(files);
        if let Some(p) = s.phases.iter_mut().find(|p| p.id == phase) {
            p.total_bytes = p.total_bytes.saturating_add(bytes);
            p.files_total = p.files_total.saturating_add(files);
        }
    }

    pub fn start_phase(&self, phase: &str) {
        let mut s = self.state.lock().unwrap();
        if let Some(p) = s.phases.iter_mut().find(|p| p.id == phase) {
            // 「跳过」也允许被重新点亮：一批跑完后空的阶段会被标成跳过，
            // 但安装器（Forge / NeoForge）是后面单独跑起来的。
            if matches!(p.status, PhaseStatus::Pending | PhaseStatus::Skipped) {
                p.status = PhaseStatus::Running;
            }
            s.current = p.label.clone();
        }
    }

    /// 一批下载结束后调用：从头到尾没轮到一个文件的阶段标成「跳过」。
    /// 有文件的阶段不动——模组是一个文件一批地下载，不能把它们打成跳过。
    pub fn skip_untouched_phases(&self) {
        let mut s = self.state.lock().unwrap();
        for p in &mut s.phases {
            if p.status == PhaseStatus::Pending && p.files_total == 0 && p.total_bytes == 0 {
                p.status = PhaseStatus::Skipped;
            }
        }
    }

    pub fn complete_phase(&self, phase: &str) {
        let mut s = self.state.lock().unwrap();
        if let Some(p) = s.phases.iter_mut().find(|p| p.id == phase) {
            if p.files_total == 0 && p.total_bytes == 0 {
                p.status = PhaseStatus::Skipped;
            } else {
                p.status = PhaseStatus::Completed;
            }
        }
    }

    pub fn begin_item(&self, phase: &str, label: &str) {
        let mut s = self.state.lock().unwrap();
        s.status = DownloadStatus::Running;
        s.current = label.to_string();
        if let Some(p) = s.phases.iter_mut().find(|p| p.id == phase) {
            if p.status == PhaseStatus::Pending {
                p.status = PhaseStatus::Running;
            }
        }
    }

    /// 文件完成：字节数已由 `add_bytes` 流式累计，这里只记文件计数。
    pub fn item_done(&self, phase: &str) {
        let mut s = self.state.lock().unwrap();
        s.files_done = s.files_done.saturating_add(1);
        if let Some(p) = s.phases.iter_mut().find(|p| p.id == phase) {
            p.files_done = p.files_done.saturating_add(1);
        }
    }

    /// 失败重试时回滚已经记入的字节，避免进度条越过 100%。
    pub fn sub_bytes(&self, phase: &str, bytes: u64) {
        let mut s = self.state.lock().unwrap();
        s.done_bytes = s.done_bytes.saturating_sub(bytes);
        if let Some(p) = s.phases.iter_mut().find(|p| p.id == phase) {
            p.done_bytes = p.done_bytes.saturating_sub(bytes);
        }
    }

    pub fn add_bytes(&self, phase: &str, bytes: u64) {
        let mut s = self.state.lock().unwrap();
        s.done_bytes = s.done_bytes.saturating_add(bytes);
        if let Some(p) = s.phases.iter_mut().find(|p| p.id == phase) {
            p.done_bytes = p.done_bytes.saturating_add(bytes);
        }
        s.window_bytes = s.window_bytes.saturating_add(bytes);
        self.evaluate_window(&mut s);
    }

    /// 即使连接完全卡住、没有任何字节到达，也按 3 秒窗口测速。
    pub fn tick(&self) {
        let mut s = self.state.lock().unwrap();
        if !matches!(
            s.status,
            DownloadStatus::Queued | DownloadStatus::Running | DownloadStatus::Resolving
        ) {
            return;
        }
        self.evaluate_window(&mut s);
    }

    fn evaluate_window(&self, s: &mut JobState) {
        let elapsed = s.window_start.elapsed();
        if elapsed < Duration::from_secs(3) {
            return;
        }
        let speed = s.window_bytes as f64 / elapsed.as_secs_f64();
        if speed < 512.0 * 1024.0 {
            s.slow_windows = s.slow_windows.saturating_add(1);
        } else {
            s.slow_windows = 0;
        }
        if self.source_policy == kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_FIRST
            && s.source != "bmclapi"
            && s.slow_windows >= 2
        {
            s.source = "bmclapi".into();
            s.current = "官方源较慢，已切换 BMCLAPI".into();
            s.slow_windows = 0;
        }
        s.window_start = Instant::now();
        s.window_bytes = 0;
    }

    pub fn reset_speed_window(&self) {
        let mut s = self.state.lock().unwrap();
        s.window_start = Instant::now();
        s.window_bytes = 0;
        s.slow_windows = 0;
    }

    pub fn switch_to_mirror(&self, reason: &str) {
        let mut s = self.state.lock().unwrap();
        if self.source_policy == kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_FIRST && s.source != "bmclapi"
        {
            s.source = "bmclapi".into();
            s.current = reason.to_string();
        }
    }

    pub fn snapshot(&self) -> DownloadJob {
        let s = self.state.lock().unwrap();
        let percent = if s.status == DownloadStatus::Completed {
            100.0
        } else if s.total_bytes > 0 {
            (s.done_bytes as f64 / s.total_bytes as f64 * 100.0).min(99.9)
        } else if s.files_total > 0 {
            (s.files_done as f64 / s.files_total as f64 * 100.0).min(99.9)
        } else {
            0.0
        };
        DownloadJob {
            id: self.id.clone(),
            title: self.title.clone(),
            version_name: self.version_name.clone(),
            root_id: self.root_id.clone(),
            game_version: self.game_version.clone(),
            loader_kind: self.loader_kind,
            loader_version: self.loader_version.clone(),
            source: s.source.clone(),
            status: s.status,
            current: s.current.clone(),
            done_bytes: s.done_bytes,
            total_bytes: s.total_bytes,
            files_done: s.files_done,
            files_total: s.files_total,
            percent,
            error: s.error.clone(),
            phases: s
                .phases
                .iter()
                .map(|p| DownloadPhase {
                    id: p.id.clone(),
                    label: p.label.clone(),
                    status: p.status,
                    done_bytes: p.done_bytes,
                    total_bytes: p.total_bytes,
                    files_done: p.files_done,
                    files_total: p.files_total,
                })
                .collect(),
            created_at: self.created_at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DownloadItem {
    pub label: String,
    pub phase: String,
    pub official_url: Option<String>,
    pub mirror_url: Option<String>,
    pub fallback_url: Option<String>,
    pub dest: PathBuf,
    pub size: u64,
    pub sha1: Option<String>,
}

#[derive(Clone)]
pub struct DownloadManager {
    executor: Arc<Executor>,
    jobs: Arc<Mutex<HashMap<String, Arc<Job>>>>,
    settings: Arc<Settings>,
    /// 所有下载共用的 HTTP agent：超时、UA 与连接复用只在这里定义一次。
    agent: Arc<ureq::Agent>,
    last_snapshot: Arc<Mutex<(Instant, u64)>>,
    /// 当前真正在跑的网络连接数（单流 1 条，分段每段 1 条）。
    /// 界面上的「N/64 线程」读的就是它——旧实现只数线程池里忙碌的 worker，
    /// 分段起的连接完全看不见，于是只剩一个文件时永远显示「1/64」。
    connections: Arc<AtomicUsize>,
}

impl DownloadManager {
    pub fn new(settings: Arc<Settings>) -> Self {
        let concurrency = settings
            .global()
            .map(|g| g.download_concurrency as usize)
            .unwrap_or(8);
        DownloadManager {
            executor: Arc::new(Executor::new(concurrency)),
            jobs: Arc::new(Mutex::new(HashMap::new())),
            settings,
            agent: Arc::new(download_agent()),
            last_snapshot: Arc::new(Mutex::new((Instant::now(), 0))),
            connections: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn register(&self, job: Arc<Job>) {
        // 用户改完并发后可能立刻开始安装；不要等 UI 下一次 poll 才扩线程池。
        self.sync_concurrency();
        self.jobs.lock().unwrap().insert(job.id.clone(), job);
    }

    pub fn job(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.lock().unwrap().get(id).cloned()
    }

    pub fn sync_concurrency(&self) {
        if let Ok(g) = self.settings.global() {
            self.executor.set_desired(g.download_concurrency as usize);
        }
    }

    /// 当前设置的下载源策略，新建任务时用它决定先试官方还是先试镜像。
    ///
    /// 启动时的「补全缺失文件」也要建任务，但那边手上没有设置对象，所以从下载器借。
    pub fn source_policy(&self) -> String {
        self.settings
            .global()
            .map(|g| g.download_source)
            .unwrap_or_else(|_| kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_FIRST.to_string())
    }

    pub fn snapshot(&self) -> DownloadSnapshot {
        self.sync_concurrency();
        let jobs: Vec<Arc<Job>> = self.jobs.lock().unwrap().values().cloned().collect();
        let mut out = Vec::with_capacity(jobs.len());
        let mut done_bytes = 0u64;
        let mut total_bytes = 0u64;
        let mut active_jobs = 0usize;
        let mut queued_jobs = 0usize;
        for job in &jobs {
            let s = job.snapshot();
            done_bytes = done_bytes.saturating_add(s.done_bytes);
            total_bytes = total_bytes.saturating_add(s.total_bytes);
            match s.status {
                DownloadStatus::Running => active_jobs += 1,
                DownloadStatus::Queued | DownloadStatus::Resolving => queued_jobs += 1,
                _ => {}
            }
            out.push(s);
        }
        out.sort_by_key(|j| std::cmp::Reverse(j.created_at));
        let percent = if total_bytes > 0 {
            (done_bytes as f64 / total_bytes as f64 * 100.0).min(100.0)
        } else if out.iter().any(|j| j.status == DownloadStatus::Completed) {
            100.0
        } else {
            0.0
        };
        let now = Instant::now();
        let mut last = self.last_snapshot.lock().unwrap();
        let elapsed = now.duration_since(last.0).as_secs_f64();
        let speed = if elapsed > 0.2 && done_bytes >= last.1 {
            ((done_bytes - last.1) as f64 / elapsed) as u64
        } else {
            0
        };
        *last = (now, done_bytes);
        DownloadSnapshot {
            jobs: out,
            done_bytes,
            total_bytes,
            percent,
            speed_bps: speed,
            concurrency: self.executor.desired.load(Ordering::SeqCst) as u32,
            active_workers: self.connections.load(Ordering::SeqCst) as u32,
            active_jobs,
            queued_jobs,
        }
    }

    pub fn cancel(&self, id: &str) -> ApiResult<()> {        let job = self
            .job(id)
            .ok_or_else(|| ApiError::not_found("下载任务"))?;
        job.cancel();
        Ok(())
    }

    pub fn clear_finished(&self) {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.retain(|_, j| {
            !matches!(
                j.snapshot().status,
                DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled
            )
        });
    }

    pub fn run_batch(&self, job: Arc<Job>, items: Vec<DownloadItem>) -> ApiResult<()> {
        if job.cancelled() {
            return Err(ApiError::invalid("任务已取消"));
        }
        let count = items.len();
        if count == 0 {
            return Ok(());
        }
        let (tx, rx) = bounded::<Result<(), String>>(count);
        job.reset_speed_window();
        let monitor_stop = Arc::new(AtomicBool::new(false));
        let monitor = {
            let stop = monitor_stop.clone();
            let watched = job.clone();
            thread::Builder::new()
                .name("kaleid-download-monitor".into())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(500));
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        watched.tick();
                    }
                })
                .ok()
        };
        for item in items {
            if job.cancelled() {
                break;
            }
            let job = job.clone();
            let tx = tx.clone();
            let agent = self.agent.clone();
            let connections = self.connections.clone();
            let executor = self.executor.clone();
            self.executor.submit(Box::new(move || {
                // 并发数在真正开跑时再读：用户在下载途中调大也会立刻生效。
                let result = download_one(
                    &agent,
                    &job,
                    &item,
                    &connections,
                    executor.concurrency(),
                );
                let _ = tx.send(result);
            }));
        }
        drop(tx);
        let mut first_error: Option<String> = None;
        for _ in 0..count {
            match rx.recv() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                        job.fail(first_error.clone().unwrap());
                    }
                }
                Err(_) => break,
            }
        }
        monitor_stop.store(true, Ordering::SeqCst);
        if let Some(handle) = monitor {
            let _ = handle.join();
        }
        if job.cancelled() {
            return Err(ApiError::invalid("任务已取消"));
        }
        if let Some(e) = first_error {
            return Err(ApiError::Network(e));
        }
        // 这一批跑完后，"一个文件都没轮到"的阶段不该继续挂着「等待」——
        // 安装已经到 99.8% 了，界面上还写着「加载器 等待」，看着像卡住。
        job.skip_untouched_phases();
        Ok(())
    }
}

/// 一条任务里该按什么顺序试 URL。
///
/// 策略取值就用设置里的四态（`official-first` / `mirror-first` / `official-only` /
/// `mirror-only`）；模组任务的策略多一个 `mod-` 前缀，排序规则完全一样，只是**不参与**
/// 慢速自动换源（BMCLAPI 上没有模组文件）。
///
/// - `source == "bmclapi"`：这条任务当前已切到镜像（官方优先被慢速监控切过来的），
///   所以镜像排前面；
/// - `mirror-only`：只给镜像；`official-only`：只给官方；
/// - 官方优先 / 镜像优先：按名字排序，另一边兜底。
fn ordered_urls(
    official: Option<&str>,
    mirror: Option<&str>,
    fallback: Option<&str>,
    source: &str,
    policy: &str,
) -> Vec<String> {
    let policy = policy.trim_start_matches("mod-");
    let mirror_only = policy == kaleid_api::DOWNLOAD_SOURCE_MIRROR_ONLY;
    let official_only = policy == kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_ONLY;
    let mirror_first =
        mirror_only || policy == kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST || source == "bmclapi";

    let mut urls = Vec::new();
    if mirror_first {
        if let Some(mirror) = mirror {
            urls.push(mirror.to_string());
        }
        // 「仅镜像」连官方地址都不碰；其它情况官方做兜底。
        if !mirror_only {
            if let Some(official) = official {
                urls.push(official.to_string());
            }
        }
    } else {
        if let Some(official) = official {
            urls.push(official.to_string());
        }
        // 选了「仅官方」就不碰镜像；但如果压根没有官方地址（CurseForge 文件就是这样），
        // 镜像就是唯一选择，仍然要给。
        if !official_only || official.is_none() {
            if let Some(mirror) = mirror {
                urls.push(mirror.to_string());
            }
        }
    }
    if let Some(fallback) = fallback {
        if !urls.iter().any(|url| url == fallback) {
            urls.push(fallback.to_string());
        }
    }
    urls
}

// ---------------------------------------------------------------------------
// 失败分类、重试、分段
// ---------------------------------------------------------------------------
//
// 这里的存在理由是一次真实故障：装 Fabric 时「Fabric API」只是一个文件，所以它
// 独占 1 个 worker——观测上就是"整批只剩一个线程在跑，然后 (os error 10060)"。
// 三件事都得补上：
//
// 1. **重试**：一次连接超时不该等于整次安装失败；
// 2. **换源**：Fabric API 的文件在 cdn.modrinth.com，而 MCIM 镜像能拿到同一份，
//    连不上时应当自动换过去（URL 顺序仍由下载源策略决定，见 `ordered_urls`）；
// 3. **分段**：单文件按 HTTP Range 切段并发，别让"一个文件"等于"一个线程"。

/// 超过这个大小才切段：版本 JSON / 资源索引这类小文件切段只多几次握手。
const SEGMENT_THRESHOLD_BYTES: u64 = 1024 * 1024;
/// 单文件最多几段。再往上收益递减：瓶颈通常是服务器侧限速。
///
/// 取 8 而不是 4：只剩一个文件时（典型就是装 Fabric 的最后一步「Fabric API」），
/// 并发设多大都只有 4 条连接在跑，剩下几十条线程干看着——用户看到的正是"1/64 线程"。
/// 上限提高到 8 后，单文件能把空闲线程用掉一部分，同时不至于对镜像站开太多连接。
const SEGMENT_MAX: usize = 8;
/// 连不上（10060 / DNS / 握手超时）时同一地址只再试 1 次就换源——
/// 真正耗时的是"等一个连不上的地址"，重试次数多只会让用户干等。
const RETRY_CONNECT_ATTEMPTS: usize = 1;
const RETRY_CONNECT_BACKOFF_MS: u64 = 600;
/// 连上后中断（读超时、连接重置、5xx）值得退避重试，这是抖动型失败。
const RETRY_TRANSFER_BACKOFF_MS: [u64; 3] = [400, 1200, 3000];

/// 一次请求失败的类型，决定「同一地址还试几次」和「要不要立刻换源」。
///
/// | 类型 | 同地址重试 | 之后 |
/// |---|---|---|
/// | `Connect` 连不上 | 1 次（600ms） | 换下一个地址 |
/// | `Transfer` 连上后中断 / 5xx | 3 次（0.4s / 1.2s / 3s） | 换下一个地址 |
/// | `Content` 大小或 SHA-1 不符 | 0 次 | 换下一个地址（重试只会再拿到错内容） |
/// | `Http` 404 / 403 / 地址非法 | 0 次 | 换下一个地址 |
#[derive(Clone, Copy)]
enum RetryClass {
    Connect,
    Transfer,
    Content,
    Http,
}

impl RetryClass {
    /// 队列里给用户看的一句话；技术细节留在最终错误串里。
    fn label(self) -> &'static str {
        match self {
            RetryClass::Connect => "连接失败",
            RetryClass::Transfer => "传输中断",
            RetryClass::Content => "内容校验失败",
            RetryClass::Http => "地址不可用",
        }
    }

    /// 同一地址还能再试几次；`0` 表示直接换下一个地址。
    fn budget(self) -> usize {
        match self {
            RetryClass::Connect => RETRY_CONNECT_ATTEMPTS,
            RetryClass::Transfer => RETRY_TRANSFER_BACKOFF_MS.len(),
            RetryClass::Content | RetryClass::Http => 0,
        }
    }
}

/// 一次尝试的失败：分类决定怎么重试，`raw` 是给排查用的原始信息。
struct AttemptError {
    class: RetryClass,
    raw: String,
}

/// 下载专用 agent：连接 10s / 读取 30s。
///
/// 旧的 `ureq::get(url).timeout(60s)` 是"整请求 60 秒"，既盖住了连接阶段（连不上要
/// 干等到超时），也让慢速大文件有被整体判死的风险；拆成连接/读取两段更贴近现实。
fn download_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent(crate::USER_AGENT)
        .build()
}

fn classify_call_error(err: &ureq::Error) -> RetryClass {
    match err {
        ureq::Error::Status(code, _) => {
            if *code >= 500 {
                RetryClass::Transfer
            } else {
                RetryClass::Http
            }
        }
        ureq::Error::Transport(t) => match t.kind() {
            ureq::ErrorKind::ConnectionFailed
            | ureq::ErrorKind::Dns
            | ureq::ErrorKind::ProxyConnect
            | ureq::ErrorKind::ProxyUnauthorized => RetryClass::Connect,
            ureq::ErrorKind::InvalidUrl
            | ureq::ErrorKind::UnknownScheme
            | ureq::ErrorKind::InvalidProxyUrl
            | ureq::ErrorKind::InsecureRequestHttpsOnly
            | ureq::ErrorKind::TooManyRedirects => RetryClass::Http,
            _ => RetryClass::Transfer,
        },
    }
}

/// 把底层网络错误翻成人话，并保留原始串（`os error 10060` 对用户毫无意义，
/// 但排查时又要能拿到）。认不出来就原样返回。
fn explain_net_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let hint = if lower.contains("10060") {
        Some("连接超时（10060）：网络或代理不通")
    } else if lower.contains("10054") || lower.contains("connection reset") {
        Some("连接被重置（10054）")
    } else if lower.contains("timed out") || lower.contains("timeout") {
        Some("连接或读取超时")
    } else if lower.contains("certificate") || lower.contains("handshake") {
        Some("TLS 握手失败（证书或代理拦截）")
    } else if lower.contains("dns") || lower.contains("name or service not known") {
        Some("域名解析失败")
    } else {
        None
    };
    match hint {
        Some(hint) => format!("{hint}（原始：{raw}）"),
        None => raw.to_string(),
    }
}

fn sleep_backoff(class: RetryClass, attempt: usize) {
    let base = match class {
        RetryClass::Connect => RETRY_CONNECT_BACKOFF_MS,
        RetryClass::Transfer => RETRY_TRANSFER_BACKOFF_MS
            [attempt.saturating_sub(1).min(RETRY_TRANSFER_BACKOFF_MS.len() - 1)],
        RetryClass::Content | RetryClass::Http => 0,
    };
    if base == 0 {
        return;
    }
    // ±20% 抖动：多个线程同时失败时不要一起醒来重连。用纳秒尾数当随机源，不引 rand。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let spread = (base / 5).max(1);
    let offset = (nanos % (spread * 2 + 1)) as i64 - spread as i64;
    let ms = (base as i64 + offset).max(1) as u64;
    thread::sleep(Duration::from_millis(ms));
}

/// 切段计划：首尾相接、互不重叠、总和等于 `size`；`slots` 是这一项能拿到的连接条数，
/// 小文件（不值得为省几次往返就拆开）和只有 1 条连接时返回空，表示走单流。
fn plan_segments(size: u64, slots: usize) -> Vec<(u64, u64)> {
    if size < SEGMENT_THRESHOLD_BYTES || slots < 2 {
        return Vec::new();
    }
    let count = slots.clamp(1, SEGMENT_MAX);
    let chunk = size.div_ceil(count as u64);
    let mut out = Vec::new();
    let mut start = 0u64;
    while start < size {
        let end = (start + chunk - 1).min(size - 1);
        out.push((start, end));
        start = end + 1;
    }
    out
}

/// 连接额度租约：先把自己要用的连接条数记到全局计数上，用完自动还回去。
///
/// 必须"先占后开"：一批文件几乎同时起跑，如果各自去读一眼"池子里还剩多少"再开连接，
/// 64 个分区文件会各自以为空闲很多，一瞬间开出几百条连接。抢额度用 CAS，谁抢到算谁的。
struct ConnLease {
    counter: Arc<AtomicUsize>,
    slots: usize,
}

impl ConnLease {
    /// 最多抢 `want` 条额度（至少 1 条：worker 自己那一条）。池子已经满了就只给 1 条，
    /// 这与旧行为一致——并发设成 1 时仍然是纯单流。
    fn claim(counter: &Arc<AtomicUsize>, desired: usize, want: usize) -> ConnLease {
        let mut cur = counter.load(Ordering::SeqCst);
        loop {
            let spare = desired.saturating_sub(cur);
            let slots = spare.min(want).max(1);
            match counter.compare_exchange(cur, cur + slots, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => {
                    return ConnLease {
                        counter: counter.clone(),
                        slots,
                    }
                }
                Err(now) => cur = now,
            }
        }
    }
}

impl Drop for ConnLease {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.slots, Ordering::SeqCst);
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

fn segment_path(dest: &Path, index: usize) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(format!(".part.{index}"));
    PathBuf::from(s)
}

/// 校验 `Content-Range: bytes 0-1023/4096` 是否正好覆盖请求的区间；
/// 服务器回不来这个头（或总长对不上）就当它不支持分段。
fn content_range_matches(header: Option<&str>, total_size: u64, range: (u64, u64)) -> bool {
    let Some(header) = header else {
        return false;
    };
    let Some((unit, rest)) = header.trim().split_once(' ') else {
        return false;
    };
    if !unit.eq_ignore_ascii_case("bytes") {
        return false;
    }
    let Some((span, total)) = rest.split_once('/') else {
        return false;
    };
    let Some((start, end)) = span.split_once('-') else {
        return false;
    };
    let (Ok(start), Ok(end)) = (start.trim().parse::<u64>(), end.trim().parse::<u64>()) else {
        return false;
    };
    if total_size > 0 {
        match total.trim().parse::<u64>() {
            Ok(total) if total == total_size => {}
            _ => return false,
        }
    }
    start == range.0 && end == range.1
}

fn download_one(
    agent: &Arc<ureq::Agent>,
    job: &Arc<Job>,
    item: &DownloadItem,
    connections: &Arc<AtomicUsize>,
    desired: usize,
) -> Result<(), String> {
    if job.cancelled() {
        return Err("任务已取消".into());
    }
    if let Some(parent) = item.dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    if item.dest.is_file() {
        let size_ok = item.size == 0
            || std::fs::metadata(&item.dest)
                .map(|m| m.len() == item.size)
                .unwrap_or(false);
        if size_ok {
            job.add_bytes(&item.phase, item.size);
            job.item_done(&item.phase);
            return Ok(());
        }
    }
    let source = job.source();
    let policy = job.policy();
    let urls = ordered_urls(
        item.official_url.as_deref(),
        item.mirror_url.as_deref(),
        item.fallback_url.as_deref(),
        &source,
        &policy,
    );
    if urls.is_empty() {
        return Err(format!("{}：没有可用的下载地址", item.label));
    }
    // 只有大文件值得抢多条连接；小文件抢了也只是几次多余握手。
    let want = if item.size >= SEGMENT_THRESHOLD_BYTES {
        SEGMENT_MAX
    } else {
        1
    };
    // 租约只用来占住额度：句柄一 drop 就把连接数还回去，不需要读它。
    let mut _lease = ConnLease::claim(connections, desired, want);
    let mut ranges = plan_segments(item.size, _lease.slots);
    let mut last_err = String::new();
    for (idx, url) in urls.iter().enumerate() {
        if job.cancelled() {
            return Err("任务已取消".into());
        }
        if idx > 0 {
            // 只有「官方优先」的任务会被切标签，其余策略里这是空操作。
            job.switch_to_mirror("官方源下载失败，已尝试镜像");
        }
        if ranges.len() >= 2 {
            match segmented_download(agent, job, item, url, &ranges) {
                SegmentOutcome::Ok => {
                    job.item_done(&item.phase);
                    return Ok(());
                }
                SegmentOutcome::Unsupported => {
                    // 服务器不认 Range：把多占的额度还回去，剩下的地址都按单流走。
                    _lease = ConnLease::claim(connections, desired, 1);
                    ranges.clear();
                    job.set_current(format!("{}：服务器不支持分段，改用单线程", item.label));
                }
                SegmentOutcome::Failed(e) => {
                    last_err = e;
                    continue;
                }
            }
        }
        match fetch_one(agent, job, item, url) {
            Ok(()) => {
                job.item_done(&item.phase);
                return Ok(());
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("{}：{last_err}", item.label))
}

/// 单流下载一个地址，按 [`RetryClass`] 的预算重试。
fn fetch_one(
    agent: &Arc<ureq::Agent>,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
) -> Result<(), String> {
    let mut attempt = 0usize;
    loop {
        if job.cancelled() {
            return Err("任务已取消".into());
        }
        match stream_attempt(agent, job, item, url) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt >= err.class.budget() {
                    return Err(explain_net_error(&err.raw));
                }
                attempt += 1;
                job.set_current(format!(
                    "{}：第 {attempt} 次重试（{}）",
                    item.label,
                    err.class.label()
                ));
                sleep_backoff(err.class, attempt);
            }
        }
    }
}

/// 一次单流尝试：成功则 `.part` 已经变成目标文件，失败则清干净并把已计入的
/// 字节吐回去（否则进度条会随着重试一路虚高）。
fn stream_attempt(
    agent: &ureq::Agent,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
) -> Result<(), AttemptError> {
    let part = part_path(&item.dest);
    let _ = std::fs::remove_file(&part);
    job.begin_item(&item.phase, &item.label);
    let mut total = 0u64;
    let outcome = stream_body(agent, job, item, url, &part, &mut total);
    if outcome.is_err() {
        let _ = std::fs::remove_file(&part);
        if total > 0 {
            job.sub_bytes(&item.phase, total);
        }
    }
    outcome
}

fn stream_body(
    agent: &ureq::Agent,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
    part: &Path,
    total: &mut u64,
) -> Result<(), AttemptError> {
    let response = agent.get(url).call().map_err(|e| AttemptError {
        class: classify_call_error(&e),
        raw: format!("{url}: {e}"),
    })?;
    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(part).map_err(|e| AttemptError {
        class: RetryClass::Content,
        raw: format!("{}: {e}", part.display()),
    })?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if job.cancelled() {
            return Err(AttemptError {
                class: RetryClass::Content,
                raw: "任务已取消".into(),
            });
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(AttemptError {
                    class: RetryClass::Transfer,
                    raw: format!("{url}: {e}"),
                })
            }
        };
        file.write_all(&buf[..n]).map_err(|e| AttemptError {
            class: RetryClass::Content,
            raw: format!("{}: {e}", part.display()),
        })?;
        hasher.update(&buf[..n]);
        *total += n as u64;
        job.add_bytes(&item.phase, n as u64);
    }
    finish_file(item, part, *total, hasher.finalize())
}

/// 收尾：大小 + SHA-1 校验通过才把 `.part` 换成目标文件。
fn finish_file(
    item: &DownloadItem,
    part: &Path,
    total: u64,
    digest: impl AsRef<[u8]>,
) -> Result<(), AttemptError> {
    if item.size > 0 && total != item.size {
        return Err(AttemptError {
            class: RetryClass::Content,
            raw: format!("大小不符（期望 {}，实际 {} 字节）", item.size, total),
        });
    }
    if let Some(expected) = &item.sha1 {
        let actual = hex::encode(digest);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(AttemptError {
                class: RetryClass::Content,
                raw: format!("SHA-1 校验失败（期望 {expected}，实际 {actual}）"),
            });
        }
    }
    if item.dest.exists() {
        let _ = std::fs::remove_file(&item.dest);
    }
    std::fs::rename(part, &item.dest).map_err(|e| AttemptError {
        class: RetryClass::Content,
        raw: format!("{}: {e}", item.dest.display()),
    })
}

enum SegmentOutcome {
    Ok,
    /// 服务器忽略 `Range`（回 200）或 `Content-Range` 对不上：退回单流，不算失败。
    Unsupported,
    Failed(String),
}

enum SegmentFail {
    Unsupported,
    Attempt(AttemptError),
}

/// 一段一次尝试。失败时把自己已经计入的字节回滚——下一次尝试会从头再来。
fn segment_attempt(
    agent: &ureq::Agent,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
    range: (u64, u64),
    file_path: &Path,
    counted: &Arc<AtomicU64>,
) -> Result<(), SegmentFail> {
    let (start, end) = range;
    let mut got = 0u64;
    let outcome = (|| -> Result<(), SegmentFail> {
        let response = agent
            .get(url)
            .set("Range", &format!("bytes={start}-{end}"))
            .call()
            .map_err(|e| {
                SegmentFail::Attempt(AttemptError {
                    class: classify_call_error(&e),
                    raw: format!("{url}: {e}"),
                })
            })?;
        if response.status() != 206 {
            // 200 说明服务器把整个文件又给了一遍：再读下去就是重复下载。
            return Err(SegmentFail::Unsupported);
        }
        if !content_range_matches(response.header("Content-Range"), item.size, range) {
            return Err(SegmentFail::Unsupported);
        }
        let mut reader = response.into_reader();
        let mut file = std::fs::File::create(file_path).map_err(|e| {
            SegmentFail::Attempt(AttemptError {
                class: RetryClass::Content,
                raw: format!("{}: {e}", file_path.display()),
            })
        })?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if job.cancelled() {
                return Err(SegmentFail::Attempt(AttemptError {
                    class: RetryClass::Content,
                    raw: "任务已取消".into(),
                }));
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    return Err(SegmentFail::Attempt(AttemptError {
                        class: RetryClass::Transfer,
                        raw: format!("{url}: {e}"),
                    }))
                }
            };
            file.write_all(&buf[..n]).map_err(|e| {
                SegmentFail::Attempt(AttemptError {
                    class: RetryClass::Content,
                    raw: format!("{}: {e}", file_path.display()),
                })
            })?;
            got += n as u64;
            counted.fetch_add(n as u64, Ordering::SeqCst);
            job.add_bytes(&item.phase, n as u64);
        }
        let want = end - start + 1;
        if got != want {
            return Err(SegmentFail::Attempt(AttemptError {
                class: RetryClass::Transfer,
                raw: format!("分段 {start}–{end} 只收到 {got} / {want} 字节"),
            }));
        }
        let _ = file.flush();
        Ok(())
    })();
    if outcome.is_err() && got > 0 {
        counted.fetch_sub(got, Ordering::SeqCst);
        job.sub_bytes(&item.phase, got);
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn fetch_segment(
    agent: &ureq::Agent,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
    range: (u64, u64),
    file_path: &Path,
    counted: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
) -> Result<(), SegmentFail> {
    let mut attempt = 0usize;
    loop {
        if stop.load(Ordering::SeqCst) || job.cancelled() {
            return Err(SegmentFail::Attempt(AttemptError {
                class: RetryClass::Content,
                raw: "任务已取消".into(),
            }));
        }
        match segment_attempt(agent, job, item, url, range, file_path, counted) {
            Ok(()) => return Ok(()),
            Err(SegmentFail::Unsupported) => return Err(SegmentFail::Unsupported),
            Err(SegmentFail::Attempt(err)) => {
                if attempt >= err.class.budget() {
                    return Err(SegmentFail::Attempt(err));
                }
                attempt += 1;
                sleep_backoff(err.class, attempt);
            }
        }
    }
}

/// 大文件分段并发下载。
///
/// 段线程就地 `spawn`，**不嵌套提交进共享线程池**：8 个 item 同时分段时，
/// 池子里 8 个 worker 全在等自己的子任务就会互相饿死。
fn segmented_download(
    agent: &Arc<ureq::Agent>,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
    ranges: &[(u64, u64)],
) -> SegmentOutcome {
    job.begin_item(&item.phase, &item.label);
    job.set_current(format!(
        "{}：分段下载（{} 个连接）",
        item.label,
        ranges.len()
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let unsupported = Arc::new(AtomicBool::new(false));
    let counted = Arc::new(AtomicU64::new(0));
    let first_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut handles = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.iter().copied().enumerate() {
        let agent = agent.clone();
        let job = job.clone();
        let item = item.clone();
        let url = url.to_string();
        let path = segment_path(&item.dest, index);
        // 进闭包用一份克隆，循环体自己还要留着 `stop` / `first_error` 处理 spawn 失败。
        let seg_stop = stop.clone();
        let seg_unsupported = unsupported.clone();
        let seg_counted = counted.clone();
        let seg_error = first_error.clone();
        let spawned = thread::Builder::new()
            .name("kaleid-download-seg".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                match fetch_segment(
                    &agent,
                    &job,
                    &item,
                    &url,
                    range,
                    &path,
                    &seg_counted,
                    &seg_stop,
                ) {
                    Ok(()) => {}
                    Err(SegmentFail::Unsupported) => {
                        seg_unsupported.store(true, Ordering::SeqCst);
                        seg_stop.store(true, Ordering::SeqCst);
                    }
                    Err(SegmentFail::Attempt(err)) => {
                        seg_stop.store(true, Ordering::SeqCst);
                        if let Ok(mut slot) = seg_error.lock() {
                            if slot.is_none() {
                                *slot = Some(explain_net_error(&err.raw));
                            }
                        }
                    }
                }
            });
        match spawned {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                stop.store(true, Ordering::SeqCst);
                if let Ok(mut slot) = first_error.lock() {
                    if slot.is_none() {
                        *slot = Some(format!("无法创建下载线程：{e}"));
                    }
                }
            }
        }
    }
    for handle in handles {
        let _ = handle.join();
    }

    let rollback = |job: &Arc<Job>| {
        for index in 0..ranges.len() {
            let _ = std::fs::remove_file(segment_path(&item.dest, index));
        }
        let done = counted.swap(0, Ordering::SeqCst);
        if done > 0 {
            job.sub_bytes(&item.phase, done);
        }
    };

    if unsupported.load(Ordering::SeqCst) {
        rollback(job);
        return SegmentOutcome::Unsupported;
    }
    if let Some(err) = first_error.lock().unwrap().clone() {
        rollback(job);
        return SegmentOutcome::Failed(err);
    }
    match join_segments(item, ranges.len()) {
        Ok(()) => SegmentOutcome::Ok,
        Err(err) => {
            rollback(job);
            let _ = std::fs::remove_file(part_path(&item.dest));
            SegmentOutcome::Failed(explain_net_error(&err))
        }
    }
}

/// 把各段按序拼回一个文件，校验大小与 SHA-1 后再换成目标文件。
fn join_segments(item: &DownloadItem, count: usize) -> Result<(), String> {
    let part = part_path(&item.dest);
    let result = (|| -> Result<(), String> {
        let mut out = std::fs::File::create(&part)
            .map_err(|e| format!("{}: {e}", part.display()))?;
        let mut hasher = Sha1::new();
        let mut total = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        for index in 0..count {
            let seg = segment_path(&item.dest, index);
            let mut input =
                std::fs::File::open(&seg).map_err(|e| format!("{}: {e}", seg.display()))?;
            loop {
                let n = input
                    .read(&mut buf)
                    .map_err(|e| format!("{}: {e}", seg.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n])
                    .map_err(|e| format!("{}: {e}", part.display()))?;
                total += n as u64;
            }
        }
        out.flush().map_err(|e| format!("{}: {e}", part.display()))?;
        drop(out);
        if item.size > 0 && total != item.size {
            return Err(format!(
                "大小不符（期望 {}，实际 {} 字节）",
                item.size, total
            ));
        }
        if let Some(expected) = &item.sha1 {
            let actual = hex::encode(hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(format!("SHA-1 校验失败（期望 {expected}，实际 {actual}）"));
            }
        }
        if item.dest.exists() {
            let _ = std::fs::remove_file(&item.dest);
        }
        std::fs::rename(&part, &item.dest).map_err(|e| format!("{}: {e}", item.dest.display()))?;
        Ok(())
    })();
    for index in 0..count {
        let _ = std::fs::remove_file(segment_path(&item.dest, index));
    }
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_order_follows_source_and_policy() {
        let official = Some("https://cdn.example/mod.jar");
        let mirror = Some("https://mirror.example/mod.jar");
        let official_s = "https://cdn.example/mod.jar".to_string();
        let mirror_s = "https://mirror.example/mod.jar".to_string();

        // 官方优先：官方先试，失败再镜像。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "official-first"),
            vec![official_s.clone(), mirror_s.clone()]
        );
        // 模组任务带 `mod-` 前缀，排序规则相同（只是不会被慢速监控切源）。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "mod-official-first"),
            vec![official_s.clone(), mirror_s.clone()]
        );
        // 镜像优先：镜像先试，官方兜底。
        assert_eq!(
            ordered_urls(official, mirror, None, "bmclapi", "mirror-first"),
            vec![mirror_s.clone(), official_s.clone()]
        );
        // 仅官方：不碰镜像。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "official-only"),
            vec![official_s.clone()]
        );
        // 仅镜像：不碰官方；没有官方直链（CurseForge）时行为一致。
        assert_eq!(
            ordered_urls(official, mirror, None, "bmclapi", "mirror-only"),
            vec![mirror_s.clone()]
        );
        assert_eq!(
            ordered_urls(None, mirror, None, "bmclapi", "mirror-first"),
            vec![mirror_s.clone()]
        );
        // 「仅官方」但压根没有官方地址：镜像就是唯一选择。
        assert_eq!(
            ordered_urls(None, mirror, None, "mojang", "official-only"),
            vec![mirror_s.clone()]
        );
        // 官方优先被慢速监控切到镜像后，镜像排前面；fallback 最后且不重复。
        assert_eq!(
            ordered_urls(official, mirror, official, "bmclapi", "official-first"),
            vec![mirror_s.clone(), official_s.clone()]
        );
    }

    #[test]
    fn executor_never_exceeds_desired_workers() {
        let executor = Arc::new(Executor::new(3));
        let active = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicUsize::new(0));
        for _ in 0..12 {
            let active = active.clone();
            let max = max.clone();
            let done = done.clone();
            executor.submit(Box::new(move || {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(25));
                active.fetch_sub(1, Ordering::SeqCst);
                done.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while done.load(Ordering::SeqCst) < 12 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(done.load(Ordering::SeqCst), 12);
        let observed = max.load(Ordering::SeqCst);
        assert!(observed <= 3, "max workers {observed} > 3");
        assert!(observed >= 2, "pool did not run tasks concurrently");
    }

    #[test]
    fn job_progress_counts_streamed_bytes_once() {
        let job = Job::new(
            "j1".into(),
            "t".into(),
            "v".into(),
            "r".into(),
            "1.21.4".into(),
            LoaderKind::Vanilla,
            None,
            "mojang".into(),
        );
        job.start_phase("client");
        job.add_total("client", 10, 1);
        job.add_bytes("client", 5);
        job.item_done("client");
        let snapshot = job.snapshot();
        assert_eq!(snapshot.done_bytes, 5);
        assert_eq!(snapshot.files_done, 1);
        assert_eq!(snapshot.phases[1].done_bytes, 5);
    }

    #[test]
    fn untouched_phases_are_skipped_so_the_queue_does_not_look_stuck() {
        // 装 Fabric 时「加载器 / 安装器 / 模组」一个文件都没有；不标跳过的话，
        // 下载已经到 99.8%，界面上这几行还挂着「等待」。
        let job = Job::new(
            "phases".into(),
            "t".into(),
            "v".into(),
            "r".into(),
            "1.21.4".into(),
            LoaderKind::Vanilla,
            None,
            "mojang".into(),
        );
        job.add_total("client", 10, 1);
        job.skip_untouched_phases();
        let snapshot = job.snapshot();
        let status = |id: &str| {
            snapshot
                .phases
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.status)
                .unwrap()
        };
        assert_eq!(status("loader"), PhaseStatus::Skipped);
        assert_eq!(status("mods"), PhaseStatus::Skipped);
        // 有文件的阶段不能被误标：模组是一个文件一批地下载的。
        assert_eq!(status("client"), PhaseStatus::Pending);
        // 之后单独跑起来的安装器还能被重新点亮。
        job.start_phase("installer");
        assert_eq!(job.snapshot().phases[6].status, PhaseStatus::Running);
    }

    #[test]
    fn source_policy_sets_initial_source_and_stays_separate() {
        let cases = [
            ("official-first", "mojang"),
            ("official-only", "mojang"),
            ("mirror-first", "bmclapi"),
            ("mirror-only", "bmclapi"),
        ];
        for (policy, expected_source) in cases {
            let job = Job::new(
                format!("job-{policy}"),
                "t".into(),
                "v".into(),
                "r".into(),
                "1.21.4".into(),
                LoaderKind::Vanilla,
                None,
                policy.into(),
            );
            assert_eq!(job.policy(), policy);
            // 策略与「当前真实来源」是两件事：镜像优先的任务一上来标签就是镜像。
            assert_eq!(job.source(), expected_source, "policy={policy}");
        }
    }

    #[test]
    fn segments_cover_the_file_exactly_once() {
        for size in [
            SEGMENT_THRESHOLD_BYTES,
            SEGMENT_THRESHOLD_BYTES + 1,
            3 * 1024 * 1024 + 7,
            64 * 1024 * 1024,
        ] {
            for slots in [2usize, 3, 4, 8] {
                let ranges = plan_segments(size, slots);
                assert!(ranges.len() >= 2, "size={size} slots={slots}");
                assert_eq!(ranges[0].0, 0, "size={size} slots={slots}");
                assert_eq!(
                    ranges[ranges.len() - 1].1,
                    size - 1,
                    "size={size} slots={slots}"
                );
                let mut expected_start = 0u64;
                for (start, end) in &ranges {
                    assert_eq!(*start, expected_start, "size={size} slots={slots}");
                    assert!(end >= start, "size={size} slots={slots}");
                    expected_start = end + 1;
                }
                let total: u64 = ranges.iter().map(|(s, e)| e - s + 1).sum();
                assert_eq!(total, size, "size={size} slots={slots}");
            }
        }
        // 0 字节文件不切段（调用方会走单流）。
        assert!(plan_segments(0, 4).is_empty());
    }

    #[test]
    fn connection_leases_respect_the_concurrency_budget() {
        let counter = Arc::new(AtomicUsize::new(0));
        // 池子空着：第一个文件能吃到分段上限。
        let first = ConnLease::claim(&counter, 8, SEGMENT_MAX);
        assert_eq!(first.slots, SEGMENT_MAX.min(8));
        // 池子已经满了：后来的只能单流，但仍给 1 条——否则这个 worker 干不了活。
        let second = ConnLease::claim(&counter, 8, SEGMENT_MAX);
        assert_eq!(second.slots, 1);
        // 并发 1 时永远是单流（尊重"就用一个连接"的设置）。
        let solo = Arc::new(AtomicUsize::new(0));
        assert_eq!(ConnLease::claim(&solo, 1, SEGMENT_MAX).slots, 1);
        // 租约释放后额度必须全部还回去，否则计数会一路虚高。
        drop(first);
        drop(second);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn small_files_stay_single_stream() {
        // 阈值以下不分段：版本 JSON、资源索引这类文件拆开只会多几次握手。
        assert!(plan_segments(SEGMENT_THRESHOLD_BYTES - 1, 8).is_empty());
        assert!(plan_segments(0, 8).is_empty());
        // 到了阈值就按拿到的连接数切（8 条 → 8 段）。
        assert_eq!(plan_segments(SEGMENT_THRESHOLD_BYTES, 8).len(), SEGMENT_MAX);
        // 只有 1 条连接时不切：文件再大也尊重用户"就用一个连接"的设置。
        assert!(plan_segments(64 * 1024 * 1024, 1).is_empty());
    }

    #[test]
    fn retry_budget_matches_failure_class() {
        // 连不上只再试 1 次就换源；抖动型失败试满 3 次；内容/地址错不重试。
        assert_eq!(RetryClass::Connect.budget(), 1);
        assert_eq!(RetryClass::Transfer.budget(), 3);
        assert_eq!(RetryClass::Content.budget(), 0);
        assert_eq!(RetryClass::Http.budget(), 0);
    }

    #[test]
    fn net_errors_are_explained_in_plain_chinese() {
        let timeout = explain_net_error(
            "https://cdn.modrinth.com/x.jar: io error: os error 10060",
        );
        assert!(timeout.contains("连接超时（10060）"), "{timeout}");
        // 原始信息必须保留，排查全靠它。
        assert!(timeout.contains("os error 10060"), "{timeout}");

        let reset = explain_net_error("https://example.invalid/x: connection reset by peer");
        assert!(reset.contains("连接被重置"), "{reset}");

        // 认不出来的错误原样返回，不要硬编一句看不懂的提示。
        let unknown = explain_net_error("磁盘满了");
        assert_eq!(unknown, "磁盘满了");
    }

    #[test]
    fn content_range_header_is_validated() {
        assert!(content_range_matches(Some("bytes 0-1023/4096"), 4096, (0, 1023)));
        assert!(content_range_matches(Some("Bytes 1024-2047/4096"), 4096, (1024, 2047)));
        // 总长对不上（服务器给的是另一个文件）就当作不支持分段。
        assert!(!content_range_matches(Some("bytes 0-1023/8192"), 4096, (0, 1023)));
        // 区间对不上、单位不对、头缺失同样拒绝。
        assert!(!content_range_matches(Some("bytes 0-2047/4096"), 4096, (0, 1023)));
        assert!(!content_range_matches(Some("items 0-1023/4096"), 4096, (0, 1023)));
        assert!(!content_range_matches(None, 4096, (0, 1023)));
    }
}