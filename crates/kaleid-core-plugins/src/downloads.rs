//! 共享下载线程池与下载任务。

use std::collections::{HashMap, VecDeque};
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

use crate::hostlimit::{HostLimits, HostState};
use crate::settings::Settings;
use crate::urlcache::UrlCache;

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
    /// 先试镜像，再试官方。
    ///
    /// 只给"官方源常年连不上"的文件用——目前就是 NeoForge / Forge 的安装器：
    /// `maven.neoforged.net` 在国内经常直接超时，而玩家的下载源设置是
    /// 「官方优先」，于是安装器每次都要先白等一次超时才轮到镜像。玩家选
    /// `official-only` 时这条不生效（那时连镜像都不能碰）。
    pub prefer_mirror: bool,
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
    /// 按主机记账的并发上限与 AIMD 降速（见 [`crate::hostlimit`]）。
    limits: Arc<HostLimits>,
    /// 镜像重定向终点缓存（见 [`crate::urlcache`]）。
    ///
    /// BMCLAPI 是重定向服务：`/assets/<p>/<h>` 会跳到国内节点，`/version/<id>/client`
    /// 却跳回官方。记住终点有两点好处：分段连接直接打终点（不再每条连接都吃一次
    /// 配额），以及能判断"走镜像其实等于走官方"。
    cache: Arc<UrlCache>,
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
            limits: Arc::new(HostLimits::for_concurrency(concurrency)),
            cache: Arc::new(UrlCache::in_memory()),
        }
    }

    /// 把重定向缓存落到 `<data_root>/download-cache`，跨进程复用（TTL 6 小时）。
    pub fn with_data_root(mut self, data_root: &Path) -> Self {
        self.cache = Arc::new(UrlCache::open(&data_root.join("download-cache")));
        self
    }

    /// 读一下各主机的记账状态（诊断/界面用）。
    pub fn host_state(&self, host: &str) -> Option<HostState> {
        self.limits.state_of(host)
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
        if items.is_empty() {
            return Ok(());
        }
        let session = self.begin_session(job);
        for item in items {
            session.push(item)?;
        }
        session.close_and_wait()
    }

    /// 可追加的下载会话。安装流程先推客户端和库，资源索引解析完再追加，不必等全部清单。
    pub fn begin_session(&self, job: Arc<Job>) -> DownloadSession {
        let (cmd_tx, cmd_rx) = unbounded::<SessionCmd>();
        let (result_tx, result_rx) = bounded::<Result<(), String>>(1);
        let executor = self.executor.clone();
        let agent = self.agent.clone();
        let connections = self.connections.clone();
        let limits = self.limits.clone();
        let cache = self.cache.clone();
        thread::Builder::new()
            .name("kaleid-download-sched".into())
            .spawn(move || {
                let result = drive_session(job, executor, agent, connections, limits, cache, cmd_rx);
                let _ = result_tx.send(result);
            })
            .expect("无法创建下载调度线程");
        DownloadSession { cmd_tx, result_rx }
    }
}

/// 安装线程把文件推进去，调度线程负责把空闲连接分出去。
pub struct DownloadSession {
    cmd_tx: Sender<SessionCmd>,
    result_rx: Receiver<Result<(), String>>,
}

impl DownloadSession {
    pub fn push(&self, item: DownloadItem) -> ApiResult<()> {
        self.cmd_tx
            .send(SessionCmd::Push(item))
            .map_err(|_| ApiError::io("下载调度已结束"))
    }

    pub fn close_and_wait(self) -> ApiResult<()> {
        let _ = self.cmd_tx.send(SessionCmd::Close);
        match self.result_rx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => {
                if message == "任务已取消" {
                    Err(ApiError::invalid(message))
                } else {
                    Err(ApiError::Network(message))
                }
            }
            Err(_) => Err(ApiError::io("下载调度线程意外退出")),
        }
    }
}

enum SessionCmd {
    Push(DownloadItem),
    Close,
}

enum WorkReport {
    SingleOk(u64),
    SingleErr(u64, String),
    /// 被限流（429/503）：不是这个地址坏了，冷却后重试同一地址。
    SingleThrottled(u64, String),
    DiscoveredSize(u64, u64),
    RangeOk(u64, u64, u64),
    RangeUnsupported(u64),
    RangeErr(u64, (u64, u64), String),
    /// 某个分段被限流：区间放回队列，同样不换源。
    RangeThrottled(u64, (u64, u64), String),
}

/// 保证每个下载任务**一定**回一份报告（panic 时由 `Drop` 兜底）。
///
/// [`Executor`] 用 `catch_unwind` 吞掉了任务里的 panic：线程活着，但那条任务的
/// 报告永远发不出来。于是 `inflight` 和连接计数都下不去，整个任务会停在
/// 「0 线程 / 文件 0/N / 进度不动」。这里把"还配额 + 发报告"绑在 Drop 上，
/// panic 也照样收尾。
struct ReportGuard {
    tx: Option<Sender<WorkReport>>,
    limits: Arc<HostLimits>,
    host: String,
    id: u64,
    /// 分段任务的区间：panic 时要按区间把活放回去，不能当成整个文件失败。
    range: Option<(u64, u64)>,
}

impl ReportGuard {
    fn new(
        tx: Sender<WorkReport>,
        limits: Arc<HostLimits>,
        host: String,
        id: u64,
        range: Option<(u64, u64)>,
    ) -> Self {
        ReportGuard {
            tx: Some(tx),
            limits,
            host,
            id,
            range,
        }
    }

    /// 正常收尾：先还主机配额，再发报告。
    fn finish(mut self, report: WorkReport, ok: bool) {
        self.limits.release(&self.host);
        if ok {
            self.limits.note_ok(&self.host);
        }
        self.send(report);
    }

    fn send(&mut self, report: WorkReport) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(report);
        }
    }
}

impl Drop for ReportGuard {
    fn drop(&mut self) {
        if self.tx.is_none() {
            return;
        }
        self.limits.release(&self.host);
        let message = "下载线程异常退出（内部错误）".to_string();
        let report = match self.range {
            Some(range) => WorkReport::RangeErr(self.id, range, message),
            None => WorkReport::SingleErr(self.id, message),
        };
        self.send(report);
    }
}

struct LiveFile {
    id: u64,
    item: DownloadItem,
    urls: Vec<String>,
    url_index: usize,
    unassigned: VecDeque<(u64, u64)>,
    inflight: usize,
    size: u64,
    /// 文件大到值得切段（与服务端是否支持 Range 无关）。
    multi: bool,
    /// 每个地址各自认不认 `Range`。按地址记而不是按文件记，是因为镜像换过去
    /// 之后能力可能完全不同——旧实现把结果钉在文件上，一换源就永远单线程。
    capability: HashMap<String, bool>,
    segments: Vec<(u64, u64)>,
    finished: bool,
    failed: Option<String>,
    single_started: bool,
    /// 同一个地址已经被限流过几轮。轮满了才认输换源，避免"限速 = 立刻换源"。
    throttle_rounds: usize,
    /// 该换源了，但还有连接在跑：等它们收尾再换，免得两份来源的字节混在一起。
    pending_switch: bool,
}

impl LiveFile {
    /// 当前在用的地址。
    fn current_url(&self) -> Option<&String> {
        if self.urls.is_empty() {
            return None;
        }
        self.urls.get(self.url_index.min(self.urls.len() - 1))
    }

    /// 当前地址对 `Range` 的态度：`None` 表示还没试过。
    fn range_known(&self) -> Option<bool> {
        self.capability.get(self.current_url()?).copied()
    }

    /// 现在可以按分段落任务吗。
    fn multi_active(&self) -> bool {
        self.multi && self.range_known() != Some(false)
    }

    /// 大文件但还没探测过 Range：先只放 1 条连接试探。
    fn needs_probe(&self) -> bool {
        self.multi && self.range_known().is_none()
    }

    /// 这个文件现在**还有活可干**吗（能再派一条连接出去）。
    ///
    /// 调度器每轮都靠它决定要不要给这个文件排任务。一个没下完的文件如果在这里
    /// 返回 `false`，那就再也没人管它了——队列会永远停在"0 线程、文件 0/1"。
    /// 所以这个函数同时也是"状态机有没有把活弄丢"的判据（见 [`drive_session`]）。
    fn can_progress(&self) -> bool {
        if self.finished || self.failed.is_some() {
            return true;
        }
        if self.inflight > 0 || self.pending_switch {
            return true;
        }
        if self.multi_active() {
            return !self.unassigned.is_empty();
        }
        // 单流：只要还没开过车就能重来一次。
        !self.single_started
    }
}

fn drive_session(
    job: Arc<Job>,
    executor: Arc<Executor>,
    agent: Arc<ureq::Agent>,
    connections: Arc<AtomicUsize>,
    limits: Arc<HostLimits>,
    cache: Arc<UrlCache>,
    cmd_rx: Receiver<SessionCmd>,
) -> Result<(), String> {
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
    let (done_tx, done_rx) = unbounded::<WorkReport>();
    let mut files: Vec<LiveFile> = Vec::new();
    let mut next_id = 1u64;
    let mut closed = false;
    let mut first_error: Option<String> = None;
    let result = (|| -> Result<(), String> {
        loop {
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    SessionCmd::Push(item) => {
                        files.push(accept_file(
                            &job,
                            item,
                            next_id,
                            executor.concurrency(),
                            &cache,
                            &limits,
                        ));
                        next_id += 1;
                    }
                    SessionCmd::Close => closed = true,
                }
            }
            assign_work(
                &job,
                &executor,
                &agent,
                &connections,
                &limits,
                &done_tx,
                &mut files,
            );
            if job.cancelled() && files.iter().all(|f| f.inflight == 0) {
                return Err("任务已取消".into());
            }
            // 既没有在途连接、也没有任何一个文件还能再派活 → 状态机把活弄丢了。
            // 再等下去就是永远等下去（界面表现：线程 0、文件 0/1、进度一动不动），
            // 宁可报一个能重试的错，也不能把整个安装挂在这里。
            if files.iter().all(|file| file.inflight == 0)
                && files
                    .iter()
                    .any(|file| !file.finished && file.failed.is_none())
                && !files.iter().any(LiveFile::can_progress)
            {
                return Err(
                    "下载线程卡住了：没有任何可继续的任务（请重试，通常会直接成功）".into(),
                );
            }
            if closed
                && files.iter().all(|f| f.finished || f.failed.is_some())
                && files.iter().all(|f| f.inflight == 0)
            {
                break;
            }
            let cmd_wait = cmd_rx.clone();
            let done_wait = done_rx.clone();
            crossbeam_channel::select! {
                recv(done_wait) -> report => {
                    if let Ok(report) = report {
                        connections.fetch_sub(1, Ordering::SeqCst);
                        on_report(&job, &mut files, report, &mut first_error, &limits);
                    }
                }
                recv(cmd_wait) -> cmd => {
                    if let Ok(cmd) = cmd {
                        match cmd {
                            SessionCmd::Push(item) => {
                                files.push(accept_file(
                                    &job,
                                    item,
                                    next_id,
                                    executor.concurrency(),
                                    &cache,
                                    &limits,
                                ));
                                next_id += 1;
                            }
                            SessionCmd::Close => closed = true,
                        }
                    } else {
                        closed = true;
                    }
                }
                default(Duration::from_millis(40)) => {}
            }
        }
        if job.cancelled() {
            return Err("任务已取消".into());
        }
        if first_error.is_none() {
            if let Some(message) = files.iter().find_map(|file| file.failed.clone()) {
                first_error = Some(message.clone());
                job.fail(message);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        job.skip_untouched_phases();
        Ok(())
    })();
    monitor_stop.store(true, Ordering::SeqCst);
    if let Some(handle) = monitor {
        let _ = handle.join();
    }
    result
}

/// 决定一个文件按什么顺序试哪些地址。
///
/// 显式策略（官方优先 / 镜像优先 / 仅官方 / 仅镜像）完全照用户说的办，只额外做一件
/// 无害的事：把镜像地址换成它的**终点**，这样分段连接不必每条都吃一次重定向配额。
///
/// `auto` 才做"看情况挑"：解析镜像到底会去哪——
/// - 终点就是官方主机（实测 `/version/<id>/client` 会跳回 piston-data）：那就直接用
///   官方地址，省掉一次配额和一次跳转；
/// - 终点是别的节点（实测 `/assets/<p>/<h>` 跳到国内节点）：用那个节点，分段直连；
/// - 镜像主机正在冷却（刚被限流过）：先走官方。
fn plan_urls(
    job: &Arc<Job>,
    item: &DownloadItem,
    cache: &UrlCache,
    limits: &HostLimits,
) -> Vec<String> {
    // 注意看的是**策略**而不是 `job.source()`：后者是"现在实际在用哪一侧"
    // （mojang / bmclapi），会被慢速监控改写。
    let policy = job.policy().trim_start_matches("mod-").to_string();
    let auto = policy == kaleid_api::DOWNLOAD_SOURCE_AUTO;
    let current_source = job.source();
    // 自动模式在 `ordered_urls` 里按"镜像优先"起步；解析完之后再决定要不要翻到官方。
    let effective_policy = if auto {
        kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST
    } else {
        job.policy()
    };
    let base = ordered_urls(
        item.official_url.as_deref(),
        item.mirror_url.as_deref(),
        item.fallback_url.as_deref(),
        if auto {
            kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST
        } else {
            current_source.as_str()
        },
        effective_policy,
        item.prefer_mirror,
    );
    // 只有"要分段下载的大文件"才值得解析终点。
    //
    // 解析一次就是一次请求，而一个原版安装有 4000 多个资源文件——给它们每个都发一条
    // HEAD 只会把配额翻倍，换来的信息（终点是谁）对小文件毫无价值。大文件不同：
    // 8 条分段连接若各自跟一次重定向就是 8 次配额，解析一次才划算。
    if item.size < SEGMENT_THRESHOLD_BYTES {
        return base;
    }
    let Some(mirror) = item.mirror_url.as_deref() else {
        return base;
    };
    let resolved = cache.resolve(mirror);
    let mirror_final = resolved.final_url.clone();

    let mut urls: Vec<String> = Vec::with_capacity(3);
    if auto {
        // 镜像的终点和官方是同一台机器：再走镜像只是多一次跳转、多一次配额。
        let mirror_is_official = item
            .official_url
            .as_deref()
            .map(|official| cache.resolve(official).host == resolved.host)
            .unwrap_or(false);
        let cooling = limits.cooling_down(&resolved.host);
        if mirror_is_official || cooling {
            if let Some(official) = item.official_url.as_deref() {
                urls.push(official.to_string());
            }
            if !urls.iter().any(|url| url == &mirror_final) {
                urls.push(mirror_final.clone());
            }
        } else {
            urls.push(mirror_final.clone());
            if let Some(official) = item.official_url.as_deref() {
                if !urls.iter().any(|url| url == official) {
                    urls.push(official.to_string());
                }
            }
        }
    } else {
        // 显式策略：只把"原本就打算走的镜像"换成终点，顺序一个字都不改。
        for url in base {
            if url == mirror && mirror_final != mirror {
                urls.push(mirror_final.clone());
            } else {
                urls.push(url);
            }
        }
    }
    if let Some(fallback) = item.fallback_url.as_deref() {
        if !urls.iter().any(|url| url == fallback) {
            urls.push(fallback.to_string());
        }
    }
    // 去重：终点可能和另一个候选撞上（镜像跳官方时就是如此）。
    let mut seen: Vec<String> = Vec::with_capacity(urls.len());
    for url in urls {
        if !seen.iter().any(|kept| kept == &url) {
            seen.push(url);
        }
    }
    seen
}

fn accept_file(
    job: &Arc<Job>,
    item: DownloadItem,
    id: u64,
    desired: usize,
    cache: &UrlCache,
    limits: &HostLimits,
) -> LiveFile {
    if let Some(parent) = item.dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut finished = false;
    if item.dest.is_file() {
        let size_ok = item.size == 0
            || std::fs::metadata(&item.dest)
                .map(|m| m.len() == item.size)
                .unwrap_or(false);
        if size_ok {
            job.add_bytes(&item.phase, item.size);
            job.item_done(&item.phase);
            finished = true;
        }
    }
    let urls = plan_urls(job, &item, cache, limits);
    let size = item.size;
    let multi = !finished && size >= SEGMENT_THRESHOLD_BYTES && desired >= 2;
    let mut unassigned = VecDeque::new();
    if multi && size > 0 {
        unassigned.push_back((0, size - 1));
    }
    LiveFile {
        id,
        item,
        urls,
        url_index: 0,
        unassigned,
        inflight: 0,
        size,
        multi,
        capability: HashMap::new(),
        segments: Vec::new(),
        finished,
        failed: None,
        single_started: false,
        throttle_rounds: 0,
        pending_switch: false,
    }
}

fn assign_work(
    job: &Arc<Job>,
    executor: &Arc<Executor>,
    agent: &Arc<ureq::Agent>,
    connections: &Arc<AtomicUsize>,
    limits: &Arc<HostLimits>,
    done_tx: &Sender<WorkReport>,
    files: &mut [LiveFile],
) {
    if job.cancelled() {
        return;
    }
    let desired = executor.concurrency().max(1);
    let consumers: Vec<ShareConsumer> = files
        .iter()
        .map(|file| ShareConsumer {
            remaining_bytes: if file.finished || file.failed.is_some() || file.pending_switch {
                0
            } else if file.multi_active() {
                file.unassigned.iter().map(|(s, e)| e - s + 1).sum()
            } else if !file.single_started && !file.finished {
                file.size.max(1)
            } else {
                0
            },
            inflight: file.inflight,
            multi: file.multi_active() && file.range_known() == Some(true),
        })
        .collect();
    // 还没探测 Range 的大文件先只占 1 条，避免服务器不支持分段时把整文件下八遍。
    let mut targets = fair_targets(&consumers, desired);
    for (index, file) in files.iter().enumerate() {
        if file.needs_probe() {
            targets[index] = targets[index].min(file.inflight + 1);
        }
    }
    let mut deficits: Vec<(usize, usize)> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.failed.is_none() && !file.finished)
        .map(|(index, file)| (index, targets[index].saturating_sub(file.inflight)))
        .filter(|(_, deficit)| *deficit > 0)
        .collect();
    deficits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let want: usize = deficits.iter().map(|(_, deficit)| *deficit).sum();
    let mut slots = reserve_slots(connections, desired, want);
    for (index, deficit) in deficits {
        if slots == 0 {
            break;
        }
        let take = deficit.min(slots);
        let started = start_file_work(
            job,
            executor,
            agent,
            limits,
            done_tx,
            &mut files[index],
            take,
            desired,
        );
        if started < take {
            connections.fetch_sub(take - started, Ordering::SeqCst);
        }
        slots -= take;
    }
}

fn start_file_work(
    job: &Arc<Job>,
    executor: &Arc<Executor>,
    agent: &Arc<ureq::Agent>,
    limits: &Arc<HostLimits>,
    done_tx: &Sender<WorkReport>,
    file: &mut LiveFile,
    take: usize,
    budget: usize,
) -> usize {
    if file.urls.is_empty() {
        file.failed = Some(format!("{}：没有可用的下载地址", file.item.label));
        file.finished = true;
        return 0;
    }
    let url = file.urls[file.url_index.min(file.urls.len() - 1)].clone();
    if file.url_index > 0 {
        job.switch_to_mirror("官方源下载失败，已尝试镜像");
    }
    // 按主机记账：BMCLAPI 只给我们 10 次/分钟的短时配额，硬开 8 条连接只会换来 429。
    let host = crate::urlcache::host_of(&url);
    if file.multi_active() {
        let ranges = claim_ranges(&mut file.unassigned, take, budget.max(2));
        let mut started = 0usize;
        for range in ranges {
            if limits.try_acquire(&host).is_err() {
                // 这台主机暂时不让连：区间放回去，下一轮再看。
                file.unassigned.push_front(range);
                break;
            }
            let agent = agent.clone();
            let job = job.clone();
            let item = file.item.clone();
            let done_tx = done_tx.clone();
            let url = url.clone();
            let id = file.id;
            let task_job = job.clone();
            let limits = limits.clone();
            let task_host = host.clone();
            let guard = ReportGuard::new(done_tx, limits, task_host, id, Some(range));
            executor.submit(Box::new(move || {
                let report = run_range(&agent, &task_job, &item, &url, range, id);
                let ok = matches!(report, WorkReport::RangeOk(..));
                guard.finish(report, ok);
            }));
            file.inflight += 1;
            started += 1;
            job.set_current(format!(
                "{}：分段下载（{} 个连接）",
                file.item.label,
                file.inflight.max(started)
            ));
        }
        return started;
    }
    if file.single_started || file.inflight > 0 {
        return 0;
    }
    if limits.try_acquire(&host).is_err() {
        return 0;
    }
    let agent = agent.clone();
    let job = job.clone();
    let item = file.item.clone();
    let done_tx = done_tx.clone();
    let id = file.id;
    let discover = item.size == 0;
    let task_job = job.clone();
    let limits = limits.clone();
    let task_host = host.clone();
    let guard = ReportGuard::new(done_tx, limits, task_host, id, None);
    executor.submit(Box::new(move || {
        let report = if discover {
            match discover_size(&agent, &task_job, &url) {
                Ok(Some(size)) if size >= SEGMENT_THRESHOLD_BYTES => {
                    WorkReport::DiscoveredSize(id, size)
                }
                Ok(_) => run_single(&agent, &task_job, &item, &url, id),
                Err(error) => WorkReport::SingleErr(id, error),
            }
        } else {
            run_single(&agent, &task_job, &item, &url, id)
        };
        let ok = matches!(report, WorkReport::SingleOk(_) | WorkReport::DiscoveredSize(..));
        guard.finish(report, ok);
    }));
    file.inflight = 1;
    file.single_started = true;
    job.begin_item(&file.item.phase, &file.item.label);
    1
}

fn discover_size(
    agent: &ureq::Agent,
    job: &Arc<Job>,
    url: &str,
) -> Result<Option<u64>, String> {
    if job.cancelled() {
        return Err("任务已取消".into());
    }
    match agent.get(url).call() {
        Ok(response) => {
            let len = response
                .header("Content-Length")
                .and_then(|value| value.parse().ok());
            drop(response);
            Ok(len)
        }
        Err(error) => Err(explain_net_error(&format!("{url}: {error}"))),
    }
}

fn run_single(
    agent: &Arc<ureq::Agent>,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
    id: u64,
) -> WorkReport {
    match fetch_one(agent, job, item, url) {
        Ok(()) => WorkReport::SingleOk(id),
        // 限流单独走一条路：它不代表这个地址坏了，不能顺手就换源。
        Err(error) if error.class == RetryClass::Throttle => {
            WorkReport::SingleThrottled(id, explain_net_error(&error.raw))
        }
        Err(error) => WorkReport::SingleErr(id, explain_net_error(&error.raw)),
    }
}

fn run_range(
    agent: &Arc<ureq::Agent>,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
    range: (u64, u64),
    id: u64,
) -> WorkReport {
    job.begin_item(&item.phase, &item.label);
    let path = span_path(&item.dest, range.0, range.1);
    let counted = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    match fetch_segment(agent, job, item, url, range, &path, &counted, &stop) {
        Ok(()) => WorkReport::RangeOk(id, range.0, range.1),
        Err(SegmentFail::Unsupported) => {
            let _ = std::fs::remove_file(&path);
            WorkReport::RangeUnsupported(id)
        }
        Err(SegmentFail::Attempt(error)) => {
            let _ = std::fs::remove_file(&path);
            let text = explain_net_error(&error.raw);
            if error.class == RetryClass::Throttle {
                WorkReport::RangeThrottled(id, range, text)
            } else {
                WorkReport::RangeErr(id, range, text)
            }
        }
    }
}

fn on_report(
    job: &Arc<Job>,
    files: &mut [LiveFile],
    report: WorkReport,
    first_error: &mut Option<String>,
    limits: &HostLimits,
) {
    let id = match &report {
        WorkReport::SingleOk(id)
        | WorkReport::SingleErr(id, _)
        | WorkReport::SingleThrottled(id, _)
        | WorkReport::DiscoveredSize(id, _)
        | WorkReport::RangeOk(id, _, _)
        | WorkReport::RangeUnsupported(id)
        | WorkReport::RangeErr(id, _, _)
        | WorkReport::RangeThrottled(id, _, _) => *id,
    };
    let Some(file) = files.iter_mut().find(|file| file.id == id) else {
        return;
    };
    file.inflight = file.inflight.saturating_sub(1);
    match report {
        WorkReport::SingleOk(_) => {
            file.finished = true;
            job.item_done(&file.item.phase);
        }
        WorkReport::DiscoveredSize(_, size) => {
            // 清单里没给大小的文件（安装器那类）计划时按 0 字节入账，真下起来就会
            // 出现「123.6 MB / 123.3 MB」这种超过 100% 的怪相。学到真实大小就补进去。
            let extra = size.saturating_sub(file.item.size);
            if extra > 0 {
                job.add_total(&file.item.phase, extra, 0);
            }
            file.size = size;
            file.item.size = size;
            file.single_started = false;
            file.multi = true;
            file.unassigned.clear();
            file.unassigned.push_back((0, size - 1));
        }
        WorkReport::RangeOk(_, start, end) => {
            // Range 的能力按"当前地址"记：这个地址认 Range 不代表下一个也认。
            if let Some(url) = file.current_url().cloned() {
                file.capability.insert(url, true);
            }
            file.segments.push((start, end));
            let covered = file.unassigned.is_empty() && file.inflight == 0;
            let span = if file.size == 0 {
                0
            } else {
                file.segments
                    .iter()
                    .map(|(s, e)| e - s + 1)
                    .sum::<u64>()
            };
            if covered && span == file.size {
                match join_spans(&file.item, &file.segments) {
                    Ok(()) => {
                        file.finished = true;
                        job.item_done(&file.item.phase);
                    }
                    Err(error) => fail_file(job, file, first_error, error),
                }
            }
        }
        WorkReport::RangeUnsupported(_) => {
            // 只把**这个地址**标成不支持；换源之后还要重新探测一次，
            // 旧实现把 `multi` 永久钉成 false，于是一换源就永远单线程。
            if let Some(url) = file.current_url().cloned() {
                file.capability.insert(url, false);
            }
            // 还有别的连接在跑时**不能**清空待办区间：它们是按这些区间派出去的，
            // 回来时总要有个地方落账。等它们都收尾了再统一改单流。
            if file.inflight == 0 {
                file.unassigned.clear();
                discard_segments(job, file);
                file.single_started = false;
            }
        }
        WorkReport::RangeErr(_, range, error) => {
            file.unassigned.push_front(range);
            note_transfer_failure(job, file, first_error, error);
        }
        WorkReport::SingleErr(_, error) => {
            file.single_started = false;
            note_transfer_failure(job, file, first_error, error);
        }
        // 被限流：这个地址本身没坏，只是让我们慢一点。把区间放回去、地址不动，
        // 顺便让该主机的预算减半——绝不能像旧实现那样"429 就等于换源"。
        WorkReport::RangeThrottled(_, range, error) => {
            file.unassigned.push_front(range);
            note_throttled(limits, job, file, first_error, &error);
        }
        WorkReport::SingleThrottled(_, error) => {
            file.single_started = false;
            note_throttled(limits, job, file, first_error, &error);
        }
    }
    // 当前地址不认 Range：把分段的半成品丢掉，改单流。别的地址的能力记录不受影响。
    if file.range_known() == Some(false)
        && file.inflight == 0
        && !file.finished
        && file.failed.is_none()
    {
        file.unassigned.clear();
        discard_segments(job, file);
        file.single_started = false;
    }
    // 换源要等在途连接收尾：新旧来源的字节混在一起，拼出来的文件一定是坏的。
    if file.pending_switch && file.inflight == 0 && !file.finished && file.failed.is_none() {
        file.pending_switch = false;
        switch_url(job, file);
    }
    if file.failed.is_some() && file.inflight == 0 {
        file.finished = true;
        file.unassigned.clear();
    }
    // 兜底：文件没下完，却已经"没活可干"了。
    //
    // 真实故障（NeoForge 安装卡在「安装器」不动）：几条连接同时在跑，其中一条回了
    // 200 把待办区间清空，另一条紧接着回 206 又把"认 Range"标了回来。结果既没有
    // 在途连接、也没有可派的区间，调度器每轮算出 0 个任务，队列永远停住。这里按
    // 已经下好的区段**重新算出缺口**，一有活就继续跑；实在算不出就退回单流重下。
    if !file.can_progress() && file.inflight == 0 {
        replan_missing_ranges(job, file, first_error);
    }
}

/// 把"丢掉的活"重新算出来：按 `segments` 里已经下好的区段求缺口。
///
/// 缺口算得出来就塞回待办队列（已下好的字节不浪费）；一个字节都不缺就直接拼文件；
/// 拼不上或者本来就是单流，就退回从头单流下载——**宁可多下一次，也不能永远卡住**。
fn replan_missing_ranges(job: &Arc<Job>, file: &mut LiveFile, first_error: &mut Option<String>) {
    if file.size == 0 || !file.multi_active() {
        // 单流：`single_started` 还立着但连接已经没了 → 允许重新开一趟。
        file.single_started = false;
        return;
    }
    let gaps = missing_ranges(file.size, &file.segments);
    if gaps.is_empty() {
        match join_spans(&file.item, &file.segments) {
            Ok(()) => {
                file.finished = true;
                job.item_done(&file.item.phase);
            }
            Err(error) => fail_file(job, file, first_error, error),
        }
        return;
    }
    if file.segments.is_empty() {
        // 一点都没下成：没必要再分段试探，直接单流（服务端大概率不认 Range）。
        file.multi = false;
        file.unassigned.clear();
        file.single_started = false;
        return;
    }
    file.unassigned = gaps;
    file.single_started = false;
}

/// 还剩哪些字节没拿到：`[0, size)` 减去 `segments` 里已完成的区段。
fn missing_ranges(size: u64, segments: &[(u64, u64)]) -> VecDeque<(u64, u64)> {
    let mut spans = segments.to_vec();
    spans.sort_unstable();
    let mut out = VecDeque::new();
    let mut cursor = 0u64;
    for (start, end) in spans {
        if end < start {
            continue;
        }
        if start > cursor {
            out.push_back((cursor, start - 1));
        }
        cursor = cursor.max(end.saturating_add(1));
        if size > 0 && cursor >= size {
            break;
        }
    }
    if size > 0 && cursor < size {
        out.push_back((cursor, size - 1));
    }
    out
}

/// 同一个地址被限流几轮之后才认输换源。
///
/// 一轮 = [`fetch_one`] 内部已经把 800ms / 2s / 5s 三次退避用完。也就是说
/// 单纯"慢"能扛住约 23 秒的持续限流；只有对方真的不给了才换下一个地址。
const MAX_THROTTLE_ROUNDS: usize = 3;

/// 处理"被限流"的报告：不换源、记一笔账、必要时才认输。
fn note_throttled(
    limits: &HostLimits,
    job: &Arc<Job>,
    file: &mut LiveFile,
    first_error: &mut Option<String>,
    error: &str,
) {
    let url = file
        .urls
        .get(file.url_index)
        .cloned()
        .unwrap_or_default();
    // 冷却只给一个很短的提示值：真正让这台主机慢下来的是"预算减半"，而同一个文件
    // 的下一轮重试本来就要在 `fetch_one` 里按自己的退避表等——两边叠加只会让用户
    // 白等。3 秒的默认冷却在这里会变成"每个文件多等 3 秒 × 3 轮"。
    limits.note_throttled(
        &crate::urlcache::host_of(&url),
        Some(crate::hostlimit::RETRY_HINT_MS),
    );
    if file.throttle_rounds < MAX_THROTTLE_ROUNDS {
        file.throttle_rounds += 1;
        job.set_current(format!(
            "{}：服务器限速，稍后重试（{}）",
            file.item.label, file.throttle_rounds
        ));
        return;
    }
    file.throttle_rounds = 0;
    note_transfer_failure(job, file, first_error, error.to_string());
}

fn note_transfer_failure(
    job: &Arc<Job>,
    file: &mut LiveFile,
    first_error: &mut Option<String>,
    error: String,
) {
    if error == "任务已取消" || job.cancelled() {
        file.failed = Some(error);
        return;
    }
    if file.url_index + 1 < file.urls.len() {
        // 换了地址，限流计数也归零：新地址有它自己的脾气。
        file.throttle_rounds = 0;
        file.single_started = false;
        if file.inflight == 0 {
            switch_url(job, file);
        } else {
            // 还有连接在跑：等它们收尾。新旧来源的字节不能混在一个文件里。
            file.pending_switch = true;
        }
        return;
    }
    if file.inflight == 0 {
        fail_file(job, file, first_error, error);
    } else if file.failed.is_none() {
        file.failed = Some(error);
    }
}

/// 换到下一个地址，并从干净状态重来。
///
/// "干净"很关键：半个来自官方源的 span 加上半个来自镜像的 span 拼出来的文件
/// 大小对得上、SHA-1 却一定不对，用户看到的是"校验失败"而不是"换源慢了一点"。
fn switch_url(job: &Arc<Job>, file: &mut LiveFile) {
    file.url_index += 1;
    file.single_started = false;
    file.throttle_rounds = 0;
    discard_segments(job, file);
    file.unassigned.clear();
    // 新地址以前被证明不支持 Range 就仍然单流；没试过/试过能用就继续分段。
    if file.multi && file.size > 0 && file.range_known() != Some(false) {
        file.unassigned.push_back((0, file.size - 1));
    }
}

fn fail_file(
    job: &Arc<Job>,
    file: &mut LiveFile,
    first_error: &mut Option<String>,
    error: String,
) {
    let message = format!("{}：{error}", file.item.label);
    file.failed = Some(message.clone());
    file.finished = true;
    file.unassigned.clear();
    if first_error.is_none() {
        *first_error = Some(message.clone());
        job.fail(message);
    }
}

fn discard_segments(job: &Arc<Job>, file: &mut LiveFile) {
    let mut bytes = 0u64;
    for (start, end) in file.segments.drain(..) {
        bytes = bytes.saturating_add(end - start + 1);
        let _ = std::fs::remove_file(span_path(&file.item.dest, start, end));
    }
    if bytes > 0 {
        job.sub_bytes(&file.item.phase, bytes);
    }
}

fn span_path(dest: &Path, start: u64, end: u64) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(format!(".part.{start}-{end}"));
    PathBuf::from(name)
}

fn join_spans(item: &DownloadItem, spans: &[(u64, u64)]) -> Result<(), String> {
    let mut ordered = spans.to_vec();
    ordered.sort_by_key(|(start, _)| *start);
    let part = part_path(&item.dest);
    let result = (|| -> Result<(), String> {
        let mut out = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let mut hasher = Sha1::new();
        let mut total = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        for (start, end) in &ordered {
            let path = span_path(&item.dest, *start, *end);
            let mut input = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            loop {
                let n = input.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", part.display()))?;
                total += n as u64;
            }
        }
        out.flush().map_err(|e| format!("{}: {e}", part.display()))?;
        drop(out);
        if item.size > 0 && total != item.size {
            return Err(format!("大小不符（期望 {}，实际 {} 字节）", item.size, total));
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
    for (start, end) in &ordered {
        let _ = std::fs::remove_file(span_path(&item.dest, *start, *end));
    }
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&item.dest);
    }
    result
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
/// - 官方优先 / 镜像优先：按名字排序，另一边兜底；
/// - `prefer_mirror`（逐文件）：即使策略写的是"官方优先"也先试镜像。只影响这一条
///   文件，且 `official-only` 时无效——玩家明确选了"仅官方"就不能偷偷连镜像。
fn ordered_urls(
    official: Option<&str>,
    mirror: Option<&str>,
    fallback: Option<&str>,
    source: &str,
    policy: &str,
    prefer_mirror: bool,
) -> Vec<String> {
    let policy = policy.trim_start_matches("mod-");
    let mirror_only = policy == kaleid_api::DOWNLOAD_SOURCE_MIRROR_ONLY;
    let official_only = policy == kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_ONLY;
    let mirror_first = mirror_only
        || policy == kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST
        || source == "bmclapi"
        || (prefer_mirror && !official_only);

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
/// 单段最小长度。最后一段可以更短；不够切满并发时用得上的连接全部用上，不再另设 8 段上限。
const MIN_SEGMENT_BYTES: u64 = 64 * 1024;
/// 连不上（10060 / DNS / 握手超时）时同一地址只再试 1 次就换源——
/// 真正耗时的是"等一个连不上的地址"，重试次数多只会让用户干等。
const RETRY_CONNECT_ATTEMPTS: usize = 1;
const RETRY_CONNECT_BACKOFF_MS: u64 = 600;
/// 连上后中断（读超时、连接重置、5xx）值得退避重试，这是抖动型失败。
const RETRY_TRANSFER_BACKOFF_MS: [u64; 3] = [400, 1200, 3000];
/// 被限流（429/503）时的退避。比传输中断更慢一些：对方是在明说"别这么快"。
/// 对方给了 `Retry-After` / `X-RateLimit-Reset-short` 就用它的值。
const RETRY_THROTTLE_BACKOFF_MS: [u64; 3] = [800, 2000, 5000];

/// 限流退避的固定毫秒数，只给测试用（`KALEID_THROTTLE_BACKOFF_MS=20`）。
/// 生产环境不设这个变量，走上面的退避表；测试要把"三轮认输"压缩到毫秒级才跑得动。
fn throttle_backoff_ms(attempt: usize) -> u64 {
    static OVERRIDE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let fixed = *OVERRIDE.get_or_init(|| {
        std::env::var("KALEID_THROTTLE_BACKOFF_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0)
    });
    if fixed > 0 {
        return fixed;
    }
    RETRY_THROTTLE_BACKOFF_MS[attempt.saturating_sub(1).min(RETRY_THROTTLE_BACKOFF_MS.len() - 1)]
}

/// 一次请求失败的类型，决定「同一地址还试几次」和「要不要立刻换源」。
///
/// | 类型 | 同地址重试 | 之后 |
/// |---|---|---|
/// | `Connect` 连不上 | 1 次（600ms） | 换下一个地址 |
/// | `Transfer` 连上后中断 / 5xx | 3 次（0.4s / 1.2s / 3s） | 换下一个地址 |
/// | `Throttle` 429 / 503 | 3 次（0.8s / 2s / 5s，或对方指定） | **同一地址再来一轮**，轮满才换源 |
/// | `Content` 大小或 SHA-1 不符 | 0 次 | 换下一个地址（重试只会再拿到错内容） |
/// | `Http` 404 / 403 / 地址非法 | 0 次 | 换下一个地址 |
#[derive(Clone, Copy, PartialEq, Eq)]
enum RetryClass {
    Connect,
    Transfer,
    Throttle,
    Content,
    Http,
}

impl RetryClass {
    /// 队列里给用户看的一句话；技术细节留在最终错误串里。
    fn label(self) -> &'static str {
        match self {
            RetryClass::Connect => "连接失败",
            RetryClass::Transfer => "传输中断",
            RetryClass::Throttle => "服务器限速",
            RetryClass::Content => "内容校验失败",
            RetryClass::Http => "地址不可用",
        }
    }

    /// 同一地址还能再试几次；`0` 表示直接换下一个地址。
    fn budget(self) -> usize {
        match self {
            RetryClass::Connect => RETRY_CONNECT_ATTEMPTS,
            RetryClass::Transfer => RETRY_TRANSFER_BACKOFF_MS.len(),
            RetryClass::Throttle => RETRY_THROTTLE_BACKOFF_MS.len(),
            RetryClass::Content | RetryClass::Http => 0,
        }
    }
}

/// 一次尝试的失败：分类决定怎么重试，`raw` 是给排查用的原始信息。
///
/// `retry_after` 只在 `Throttle` 时出现，来自 `Retry-After` 或
/// `X-RateLimit-Reset-short`：服务器自己说了等多久，就别再用我们拍的退避表。
struct AttemptError {
    class: RetryClass,
    raw: String,
    retry_after: Option<Duration>,
}

impl AttemptError {
    fn new(class: RetryClass, raw: impl Into<String>) -> Self {
        AttemptError {
            class,
            raw: raw.into(),
            retry_after: None,
        }
    }

    fn throttled(raw: impl Into<String>, retry_after: Option<Duration>) -> Self {
        AttemptError {
            class: RetryClass::Throttle,
            raw: raw.into(),
            retry_after,
        }
    }
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
            if *code == 429 || *code == 503 {
                // 限流 / 过载：换源只会把同一个问题搬到另一个地址上。
                RetryClass::Throttle
            } else if *code == 408 {
                RetryClass::Transfer
            } else if *code >= 500 {
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

/// 把 `ureq` 的错误连同"服务器要我们等多久"一起打包成 [`AttemptError`]。
fn attempt_error(url: &str, err: &ureq::Error) -> AttemptError {
    let class = classify_call_error(err);
    let retry_after = match err {
        ureq::Error::Status(_, response) => retry_after_of(response),
        _ => None,
    };
    if class == RetryClass::Throttle {
        AttemptError::throttled(format!("{url}: {err}"), retry_after)
    } else {
        AttemptError::new(class, format!("{url}: {err}"))
    }
}

/// 服务器指定的等待时长。
///
/// `Retry-After` 支持秒数；`X-RateLimit-Reset-short`（BMCLAPI 每 60 秒一个窗口）
/// 是"再过几秒重置"的意思，语义一致，一并认。
fn retry_after_of(response: &ureq::Response) -> Option<Duration> {
    if let Some(value) = response.header("retry-after") {
        if let Some(seconds) = value.trim().parse::<u64>().ok() {
            return Some(Duration::from_secs(seconds.clamp(1, 120)));
        }
    }
    if let Some(value) = response.header("x-ratelimit-reset-short") {
        if let Some(seconds) = value.trim().parse::<u64>().ok() {
            return Some(Duration::from_secs(seconds.clamp(1, 300)));
        }
    }
    None
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

/// 第 `attempt` 次失败后睡多久。服务器给了明确等待时长就用它，别自作聪明。
fn sleep_backoff(class: RetryClass, attempt: usize, retry_after: Option<Duration>) {
    if let Some(wait) = retry_after {
        thread::sleep(wait);
        return;
    }
    let base = match class {
        RetryClass::Connect => RETRY_CONNECT_BACKOFF_MS,
        RetryClass::Transfer => RETRY_TRANSFER_BACKOFF_MS
            [attempt.saturating_sub(1).min(RETRY_TRANSFER_BACKOFF_MS.len() - 1)],
        RetryClass::Throttle => throttle_backoff_ms(attempt),
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

/// 参与公平分配的一个在途文件。`remaining_bytes` 只算还没派出去的字节。
#[derive(Clone, Debug)]
struct ShareConsumer {
    remaining_bytes: u64,
    inflight: usize,
    /// 大文件且服务器还没被判定为不支持 Range。小文件永远 false。
    multi: bool,
}

fn new_segments_possible(bytes: u64) -> usize {
    if bytes == 0 {
        0
    } else {
        bytes.div_ceil(MIN_SEGMENT_BYTES).max(1) as usize
    }
}

fn useful_connections(consumer: &ShareConsumer) -> usize {
    if consumer.remaining_bytes == 0 && consumer.inflight == 0 {
        return 0;
    }
    if !consumer.multi {
        return 1;
    }
    consumer.inflight + new_segments_possible(consumer.remaining_bytes)
}

/// 把 `budget` 条连接分给在途文件。
///
/// 先每人至多 1 条（大文件优先，避免被一长串小文件挡住），多出来的只给大文件，
/// 余数给剩余字节更多的那个。小文件封顶 1 条。分完后若有人吃不满，空位再转给还能加段的大文件。
fn fair_targets(consumers: &[ShareConsumer], budget: usize) -> Vec<usize> {
    let mut targets = vec![0usize; consumers.len()];
    if budget == 0 || consumers.is_empty() {
        return targets;
    }
    let mut active: Vec<usize> = consumers
        .iter()
        .enumerate()
        .filter(|(_, c)| c.remaining_bytes > 0 || c.inflight > 0)
        .map(|(i, _)| i)
        .collect();
    if active.is_empty() {
        return targets;
    }
    active.sort_by(|&a, &b| {
        consumers[b]
            .multi
            .cmp(&consumers[a].multi)
            .then(consumers[b].remaining_bytes.cmp(&consumers[a].remaining_bytes))
            .then(a.cmp(&b))
    });
    let multi: Vec<usize> = active
        .iter()
        .copied()
        .filter(|&index| consumers[index].multi)
        .collect();
    let singles: Vec<usize> = active
        .iter()
        .copied()
        .filter(|&index| !consumers[index].multi)
        .collect();
    // 可分段文件优先拿预算；但只要还有单流文件，就至少留一半给分段文件。
    //
    // 没有这条保底时，一个原版安装里几千个小资源会把连接预算占满——排序虽然把 multi
    // 排在前面，可"每人 1 条"之后就再没有余量，于是 28 MB 的客户端 jar 只能用一条
    // 连接慢慢爬，看起来就是"下载客户端只用了一个线程"。
    let mut multi_pot = budget;
    if !singles.is_empty() && budget >= 2 {
        multi_pot = (budget / 2).max(multi.len()).min(budget);
    }
    if !multi.is_empty() {
        // 分段文件之间公平分，余数按剩余字节从多到少发。
        let share = multi_pot / multi.len();
        let rem = multi_pot % multi.len();
        let mut ordered = multi.clone();
        ordered.sort_by(|&a, &b| {
            consumers[b]
                .remaining_bytes
                .cmp(&consumers[a].remaining_bytes)
                .then(a.cmp(&b))
        });
        for (rank, index) in ordered.into_iter().enumerate() {
            targets[index] = share + usize::from(rank < rem);
        }
    }
    let mut slots = budget.saturating_sub(targets.iter().sum::<usize>());
    // 剩下的给单流文件各 1 条。
    for &index in &singles {
        if slots == 0 {
            break;
        }
        targets[index] = 1;
        slots -= 1;
    }
    // 还有空位就补给第一个分段文件（它最吃得到连接）。
    if slots > 0 {
        if let Some(&big) = multi.first() {
            targets[big] += slots;
        }
    }
    for (index, consumer) in consumers.iter().enumerate() {
        targets[index] = targets[index].min(useful_connections(consumer));
        if !consumer.multi {
            targets[index] = targets[index].min(1);
        }
    }
    // 有人被 64 KiB 下限卡住时，把吃不满的额度转给还能加段的大文件。
    loop {
        let used: usize = targets.iter().sum();
        if used >= budget {
            break;
        }
        let mut best: Option<usize> = None;
        for (index, consumer) in consumers.iter().enumerate() {
            if !consumer.multi || targets[index] >= useful_connections(consumer) {
                continue;
            }
            best = Some(match best {
                None => index,
                Some(prev) => {
                    if consumer.remaining_bytes > consumers[prev].remaining_bytes
                        || (consumer.remaining_bytes == consumers[prev].remaining_bytes
                            && index < prev)
                    {
                        index
                    } else {
                        prev
                    }
                }
            });
        }
        let Some(index) = best else {
            break;
        };
        targets[index] += 1;
    }
    targets
}

/// 从尚未分配的区间前端切出 `need` 段。段长约为「剩余字节 / 总预算」，但不小于 64 KiB，
/// 这样当前份额不会把整个文件一次切完，同伴结束后还能把尾巴加派出去。
fn claim_ranges(
    unassigned: &mut VecDeque<(u64, u64)>,
    need: usize,
    budget: usize,
) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    if need == 0 || budget == 0 {
        return out;
    }
    while out.len() < need {
        let Some((start, end)) = unassigned.pop_front() else {
            break;
        };
        if end < start {
            continue;
        }
        let len = end - start + 1;
        let tail: u64 = unassigned.iter().map(|(s, e)| e - s + 1).sum();
        let remaining = len + tail;
        let chunk = (remaining / budget.max(1) as u64).max(MIN_SEGMENT_BYTES);
        if len <= chunk {
            out.push((start, end));
            continue;
        }
        let piece_end = start + chunk - 1;
        out.push((start, piece_end));
        unassigned.push_front((piece_end + 1, end));
    }
    out
}

fn reserve_slots(counter: &AtomicUsize, desired: usize, want: usize) -> usize {
    if want == 0 || desired == 0 {
        return 0;
    }
    let mut cur = counter.load(Ordering::SeqCst);
    loop {
        let spare = desired.saturating_sub(cur);
        let got = spare.min(want);
        if got == 0 {
            return 0;
        }
        match counter.compare_exchange(cur, cur + got, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return got,
            Err(now) => cur = now,
        }
    }
}

/// 切段计划：首尾相接、互不重叠、总和等于 `size`。只留给测试核对旧的覆盖不变量。
#[cfg_attr(not(test), allow(dead_code))]
fn plan_segments(size: u64, slots: usize) -> Vec<(u64, u64)> {
    if size < SEGMENT_THRESHOLD_BYTES || slots < 2 {
        return Vec::new();
    }
    let count = slots.max(1);
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

// ---- 区间/单流底层：调度器每个 worker 只调一次 ----

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
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


/// 单流下载一个地址，按 [`RetryClass`] 的预算重试。
///
/// 返回的 [`AttemptError`] 保留分类：调用方要据此区分"这个地址坏了"和
/// "服务器只是让我们慢一点"。
fn fetch_one(
    agent: &Arc<ureq::Agent>,
    job: &Arc<Job>,
    item: &DownloadItem,
    url: &str,
) -> Result<(), AttemptError> {
    let mut attempt = 0usize;
    loop {
        if job.cancelled() {
            return Err(AttemptError::new(RetryClass::Content, "任务已取消"));
        }
        match stream_attempt(agent, job, item, url) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt >= err.class.budget() {
                    return Err(err);
                }
                attempt += 1;
                job.set_current(format!(
                    "{}：第 {attempt} 次重试（{}）",
                    item.label,
                    err.class.label()
                ));
                sleep_backoff(err.class, attempt, err.retry_after);
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
    let response = agent.get(url).call().map_err(|e| attempt_error(url, &e))?;
    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(part).map_err(|e| AttemptError::new(RetryClass::Content, format!("{}: {e}", part.display())))?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if job.cancelled() {
            return Err(AttemptError::new(RetryClass::Content, "任务已取消"));
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(AttemptError::new(RetryClass::Transfer, format!("{url}: {e}")))
            }
        };
        file.write_all(&buf[..n]).map_err(|e| AttemptError::new(RetryClass::Content, format!("{}: {e}", part.display())))?;
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
        return Err(AttemptError::new(RetryClass::Content, format!("大小不符（期望 {}，实际 {} 字节）", item.size, total)));
    }
    if let Some(expected) = &item.sha1 {
        let actual = hex::encode(digest);
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(AttemptError::new(RetryClass::Content, format!("SHA-1 校验失败（期望 {expected}，实际 {actual}）")));
        }
    }
    if item.dest.exists() {
        let _ = std::fs::remove_file(&item.dest);
    }
    std::fs::rename(part, &item.dest).map_err(|e| AttemptError::new(RetryClass::Content, format!("{}: {e}", item.dest.display())))
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
            .map_err(|e| SegmentFail::Attempt(attempt_error(url, &e)))?;
        if response.status() != 206 {
            // 200 说明服务器把整个文件又给了一遍：再读下去就是重复下载。
            return Err(SegmentFail::Unsupported);
        }
        if !content_range_matches(response.header("Content-Range"), item.size, range) {
            return Err(SegmentFail::Unsupported);
        }
        let mut reader = response.into_reader();
        let mut file = std::fs::File::create(file_path).map_err(|e| {
            SegmentFail::Attempt(AttemptError::new(RetryClass::Content, format!("{}: {e}", file_path.display())))
        })?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if job.cancelled() {
                return Err(SegmentFail::Attempt(AttemptError::new(RetryClass::Content, "任务已取消")));
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    return Err(SegmentFail::Attempt(AttemptError::new(RetryClass::Transfer, format!("{url}: {e}"))))
                }
            };
            file.write_all(&buf[..n]).map_err(|e| {
                SegmentFail::Attempt(AttemptError::new(RetryClass::Content, format!("{}: {e}", file_path.display())))
            })?;
            got += n as u64;
            counted.fetch_add(n as u64, Ordering::SeqCst);
            job.add_bytes(&item.phase, n as u64);
        }
        let want = end - start + 1;
        if got != want {
            return Err(SegmentFail::Attempt(AttemptError::new(RetryClass::Transfer, format!("分段 {start}–{end} 只收到 {got} / {want} 字节"))));
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
            return Err(SegmentFail::Attempt(AttemptError::new(RetryClass::Content, "任务已取消")));
        }
        match segment_attempt(agent, job, item, url, range, file_path, counted) {
            Ok(()) => return Ok(()),
            Err(SegmentFail::Unsupported) => return Err(SegmentFail::Unsupported),
            Err(SegmentFail::Attempt(err)) => {
                if attempt >= err.class.budget() {
                    return Err(SegmentFail::Attempt(err));
                }
                attempt += 1;
                sleep_backoff(err.class, attempt, err.retry_after);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_job(source: &str) -> Arc<Job> {
        Job::new(
            "plan".into(),
            "t".into(),
            "v".into(),
            "r".into(),
            "1.21.4".into(),
            LoaderKind::Vanilla,
            None,
            source.into(),
        )
    }

    fn plan_item(size: u64) -> DownloadItem {
        DownloadItem {
            label: "客户端".into(),
            phase: "client".into(),
            official_url: Some("https://piston-data.mojang.com/v1/objects/abc/client.jar".into()),
            mirror_url: Some("https://bmclapi2.bangbang93.com/version/1.21.4/client".into()),
            fallback_url: None,
            prefer_mirror: false,
            dest: PathBuf::from("client.jar"),
            size,
            sha1: None,
        }
    }

    /// 一个正在分段下载、且已经有 4 条连接在跑的 LiveFile（测试里直接用，不起线程）。
    fn segmented_file(id: u64, size: u64, url: &str, capability: Option<bool>) -> LiveFile {
        let item = plan_item(size);
        let mut capability_map = HashMap::new();
        if let Some(known) = capability {
            capability_map.insert(url.to_string(), known);
        }
        LiveFile {
            id,
            item,
            urls: vec![url.to_string()],
            url_index: 0,
            unassigned: VecDeque::from(vec![(0, size.saturating_sub(1))]),
            inflight: 0,
            size,
            multi: true,
            capability: capability_map,
            segments: Vec::new(),
            finished: false,
            failed: None,
            single_started: false,
            throttle_rounds: 0,
            pending_switch: false,
        }
    }

    /// 真机故障：NeoForge 安装卡在「安装器」不动，线程 0、文件 0/1、永远不结束。
    ///
    /// 分段下载时几条连接同时在跑，其中一条回了 200（那台节点不认 Range）把整个
    /// 待办区间清空了；紧跟着另一条正常回 206，又把「这台主机认 Range」标了回去。
    /// 于是状态变成：**没有在途连接、也没有可派的区间、文件却没下完**——调度器每轮
    /// 都算出 0 个任务，队列就永远停在那里。
    #[test]
    fn a_late_range_ok_must_not_leave_the_file_without_any_work() {
        let url = "https://node.example/client.jar";
        let size = 4 * 1024 * 1024;
        let mut file = segmented_file(1, size, url, Some(true));
        file.inflight = 4;
        let mut files = vec![file];
        let job = plan_job("official-first");
        let limits = HostLimits::for_concurrency(8);
        let mut first_error = None;

        // 一条连接拿回了 200：这个地址被标记成"不认 Range"，待办区间被清空。
        on_report(
            &job,
            &mut files,
            WorkReport::RangeUnsupported(1),
            &mut first_error,
            &limits,
        );
        // 另一条连接正常拿回 206：能力又被标回"认 Range"，但待办区间还是空的。
        on_report(
            &job,
            &mut files,
            WorkReport::RangeOk(1, 0, size / 4 - 1),
            &mut first_error,
            &limits,
        );

        let file = &files[0];
        assert!(file.failed.is_none(), "不该判失败：{:?}", file.failed);
        assert!(
            file.can_progress(),
            "既没有在途连接、也没有可派的区间、还没下完 —— 调度器会算出 0 个任务，队列永远卡住"
        );
    }

    /// 学到真实大小之后，总数要跟着修正——否则进度条会跑到 100% 以上。
    ///
    /// 安装器这类文件在清单里 `size: 0`，旧实现只在文件级补了大小，批次总字节仍是
    /// 0，于是界面上出现「123.6 MB / 123.3 MB」：明明显示 100%，文件却是 0/1。
    #[test]
    fn learning_the_real_size_keeps_the_total_honest() {
        let url = "https://node.example/installer.jar";
        let job = plan_job("official-first");
        let mut file = segmented_file(1, 0, url, None);
        file.multi = false;
        file.unassigned.clear();
        job.add_total("client", 0, 1);
        let limits = HostLimits::for_concurrency(8);
        let mut files = vec![file];
        let mut first_error = None;

        on_report(
            &job,
            &mut files,
            WorkReport::DiscoveredSize(1, 2_500_000),
            &mut first_error,
            &limits,
        );

        let snapshot = job.snapshot();
        assert_eq!(snapshot.total_bytes, 2_500_000, "总数要补上现学的大小");
        assert_eq!(snapshot.files_total, 1, "文件数不能被重复计");
        assert_eq!(files[0].unassigned.len(), 1, "学到了大小就该开始分段");
    }

    /// 缺口计算：一段一段补回来，不能重叠、也不能漏。
    #[test]
    fn missing_ranges_cover_everything_that_is_still_needed() {
        let size = 100;
        assert_eq!(
            missing_ranges(size, &[]).into_iter().collect::<Vec<_>>(),
            vec![(0, 99)]
        );
        // 中间有一个洞 + 尾巴没下
        assert_eq!(
            missing_ranges(size, &[(10, 19), (0, 9)])
                .into_iter()
                .collect::<Vec<_>>(),
            vec![(20, 99)]
        );
        assert_eq!(
            missing_ranges(size, &[(0, 49), (60, 99)])
                .into_iter()
                .collect::<Vec<_>>(),
            vec![(50, 59)]
        );
        // 已经下满了
        assert!(missing_ranges(size, &[(60, 99), (0, 59)]).is_empty());
    }

    fn seeded_cache(mirror_final: &str) -> UrlCache {
        let cache = UrlCache::in_memory();
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        cache.put(
            "https://bmclapi2.bangbang93.com/version/1.21.4/client",
            crate::urlcache::Resolved {
                final_url: mirror_final.to_string(),
                hops: 2,
                host: crate::urlcache::host_of(mirror_final),
                at,
            },
        );
        cache
    }

    #[test]
    fn auto_source_drops_the_mirror_when_it_only_redirects_to_official() {
        // 实测：/version/<id>/client 最终 302 回 piston-data。走镜像等于多花一次配额。
        let cache = seeded_cache(
            "https://piston-data.mojang.com/v1/objects/abc/client.jar",
        );
        let limits = HostLimits::for_concurrency(8);
        let urls = plan_urls(
            &plan_job(kaleid_api::DOWNLOAD_SOURCE_AUTO),
            &plan_item(28 * 1024 * 1024),
            &cache,
            &limits,
        );
        assert_eq!(
            urls,
            vec!["https://piston-data.mojang.com/v1/objects/abc/client.jar".to_string()],
            "镜像只是把同一份文件转给官方时，应当直接用官方地址（并去重）"
        );
    }

    #[test]
    fn auto_source_uses_the_mirror_node_when_it_ends_elsewhere() {
        // 实测：/assets/<p>/<h> 会跳到国内节点，这时候镜像才真的有用。
        let cache = seeded_cache("https://bmclapi22.sakura.sld.tw/download/abc");
        let limits = HostLimits::for_concurrency(8);
        let urls = plan_urls(
            &plan_job(kaleid_api::DOWNLOAD_SOURCE_AUTO),
            &plan_item(28 * 1024 * 1024),
            &cache,
            &limits,
        );
        assert_eq!(
            urls,
            vec![
                "https://bmclapi22.sakura.sld.tw/download/abc".to_string(),
                "https://piston-data.mojang.com/v1/objects/abc/client.jar".to_string(),
            ],
            "分段连接应当直接打镜像的终点，官方做兜底"
        );
    }

    #[test]
    fn small_files_skip_url_resolution_entirely() {
        // 4000 个资源文件每个都发一条 HEAD 只会把配额翻倍，所以小文件不解析。
        let cache = seeded_cache("https://bmclapi22.sakura.sld.tw/download/abc");
        let limits = HostLimits::for_concurrency(8);
        let urls = plan_urls(
            &plan_job(kaleid_api::DOWNLOAD_SOURCE_AUTO),
            &plan_item(4096),
            &cache,
            &limits,
        );
        assert_eq!(
            urls,
            vec![
                "https://bmclapi2.bangbang93.com/version/1.21.4/client".to_string(),
                "https://piston-data.mojang.com/v1/objects/abc/client.jar".to_string(),
            ],
            "小文件保持原样的镜像优先顺序"
        );
    }

    #[test]
    fn explicit_source_keeps_its_order_but_uses_the_resolved_node() {
        let cache = seeded_cache("https://bmclapi22.sakura.sld.tw/download/abc");
        let limits = HostLimits::for_concurrency(8);
        let urls = plan_urls(
            &plan_job(kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST),
            &plan_item(28 * 1024 * 1024),
            &cache,
            &limits,
        );
        assert_eq!(
            urls,
            vec![
                "https://bmclapi22.sakura.sld.tw/download/abc".to_string(),
                "https://piston-data.mojang.com/v1/objects/abc/client.jar".to_string(),
            ]
        );
    }

    #[test]
    fn throttled_mirror_host_sends_auto_traffic_to_official_first() {
        let cache = seeded_cache("https://bmclapi22.sakura.sld.tw/download/abc");
        let limits = HostLimits::for_concurrency(8);
        // 假装镜像节点刚被限流、要冷却一分钟。
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        limits.note_throttled_at("bmclapi22.sakura.sld.tw", Some(60_000), now);
        assert!(limits.cooling_down("bmclapi22.sakura.sld.tw"));

        let urls = plan_urls(
            &plan_job(kaleid_api::DOWNLOAD_SOURCE_AUTO),
            &plan_item(28 * 1024 * 1024),
            &cache,
            &limits,
        );
        assert_eq!(
            urls.first().map(String::as_str),
            Some("https://piston-data.mojang.com/v1/objects/abc/client.jar"),
            "镜像正在冷却时应当先走官方，别明知会被挡还硬撞"
        );
    }

    #[test]
    fn url_order_follows_source_and_policy() {
        let official = Some("https://cdn.example/mod.jar");
        let mirror = Some("https://mirror.example/mod.jar");
        let official_s = "https://cdn.example/mod.jar".to_string();
        let mirror_s = "https://mirror.example/mod.jar".to_string();

        // 官方优先：官方先试，失败再镜像。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "official-first", false),
            vec![official_s.clone(), mirror_s.clone()]
        );
        // 模组任务带 `mod-` 前缀，排序规则相同（只是不会被慢速监控切源）。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "mod-official-first", false),
            vec![official_s.clone(), mirror_s.clone()]
        );
        // 镜像优先：镜像先试，官方兜底。
        assert_eq!(
            ordered_urls(official, mirror, None, "bmclapi", "mirror-first", false),
            vec![mirror_s.clone(), official_s.clone()]
        );
        // 仅官方：不碰镜像。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "official-only", false),
            vec![official_s.clone()]
        );
        // 仅镜像：不碰官方；没有官方直链（CurseForge）时行为一致。
        assert_eq!(
            ordered_urls(official, mirror, None, "bmclapi", "mirror-only", false),
            vec![mirror_s.clone()]
        );
        assert_eq!(
            ordered_urls(None, mirror, None, "bmclapi", "mirror-first", false),
            vec![mirror_s.clone()]
        );
        // 「仅官方」但压根没有官方地址：镜像就是唯一选择。
        assert_eq!(
            ordered_urls(None, mirror, None, "mojang", "official-only", false),
            vec![mirror_s.clone()]
        );
        // 官方优先被慢速监控切到镜像后，镜像排前面；fallback 最后且不重复。
        assert_eq!(
            ordered_urls(official, mirror, official, "bmclapi", "official-first", false),
            vec![mirror_s.clone(), official_s.clone()]
        );
    }

    /// 加载器安装器这类"官方源常年连不上"的文件要能先试镜像那条路，
    /// 但玩家选了「仅官方」时不许偷偷连镜像。
    #[test]
    fn prefer_mirror_puts_bmclapi_first_without_breaking_official_only() {
        let official =
            Some("https://maven.neoforged.net/releases/net/neoforged/neoforge/26.3.0.99/neoforge-26.3.0.99-installer.jar");
        let mirror =
            Some("https://bmclapi2.bangbang93.com/maven/net/neoforged/neoforge/26.3.0.99/neoforge-26.3.0.99-installer.jar");
        let official_s = official.unwrap().to_string();
        let mirror_s = mirror.unwrap().to_string();

        // 默认策略是「官方优先」，但这条文件先试镜像。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "official-first", true),
            vec![mirror_s.clone(), official_s.clone()]
        );
        // 「仅镜像」不变。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "mirror-only", true),
            vec![mirror_s.clone()]
        );
        // 「仅官方」压过 prefer_mirror：镜像连排都不该排进去。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "official-only", true),
            vec![official_s.clone()]
        );
        // 没标 prefer_mirror 的文件，顺序一个字都不变。
        assert_eq!(
            ordered_urls(official, mirror, None, "mojang", "official-first", false),
            vec![official_s.clone(), mirror_s.clone()]
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

    fn consumer(remaining: u64, inflight: usize, multi: bool) -> ShareConsumer {
        ShareConsumer {
            remaining_bytes: remaining,
            inflight,
            multi,
        }
    }

    #[test]
    fn fair_share_one_large_file_takes_the_whole_budget() {
        let size = 8 * 1024 * 1024;
        let targets = fair_targets(&[consumer(size, 0, true)], 64);
        let cap = (size / MIN_SEGMENT_BYTES) as usize;
        assert_eq!(targets, vec![64.min(cap)]);
    }

    #[test]
    fn fair_share_splits_evenly() {
        let size = 8 * 1024 * 1024;
        assert_eq!(
            fair_targets(&[consumer(size, 0, true), consumer(size, 0, true)], 8),
            vec![4, 4]
        );
        // 余数给剩余字节更多的文件。三个文件、预算 8 → 3、3、2。
        assert_eq!(
            fair_targets(
                &[
                    consumer(size, 0, true),
                    consumer(size / 2, 0, true),
                    consumer(size, 0, true),
                ],
                8
            ),
            vec![3, 2, 3]
        );
    }

    #[test]
    fn rebalance_after_peer_finishes() {
        let size = 8 * 1024 * 1024;
        let mut unassigned = VecDeque::from([(0u64, size - 1)]);
        let first = claim_ranges(&mut unassigned, 4, 8);
        assert_eq!(first.len(), 4);
        let consumed: u64 = first.iter().map(|(start, end)| end - start + 1).sum();
        assert!(consumed < size, "不能一次把未分配字节切完，否则无法加派");
        // 同伴结束后预算全归这个文件，只从剩下的区间加派，不重下已派出的段。
        let targets = fair_targets(&[consumer(size - consumed, 4, true)], 8);
        assert_eq!(targets, vec![8]);
        let more = claim_ranges(&mut unassigned, 4, 8);
        assert_eq!(more.len(), 4);
        assert!(more.iter().all(|(start, _)| *start >= first[0].1));
    }

    #[test]
    fn small_file_never_takes_a_second_connection() {
        let targets = fair_targets(&[consumer(512 * 1024, 0, false)], 8);
        assert_eq!(targets, vec![1]);
    }

    #[test]
    fn small_files_stay_single_stream() {
        assert!(plan_segments(SEGMENT_THRESHOLD_BYTES - 1, 8).is_empty());
        assert!(plan_segments(0, 8).is_empty());
        assert_eq!(plan_segments(SEGMENT_THRESHOLD_BYTES, 8).len(), 8);
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
