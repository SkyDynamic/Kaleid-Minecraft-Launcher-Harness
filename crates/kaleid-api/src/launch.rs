//! 真实启动的运行时状态、崩溃判定与日志导出。
//!
//! 这些结构只描述"现在游戏处于什么阶段""这次退出是不是崩溃"，**不含任何令牌**：
//! 启动脚本里出现的那份参数会把 `--accessToken` 的值替换成 `***`。

use serde::{Deserialize, Serialize};

pub use crate::download::PhaseStatus;

/// 启动阶段。
///
/// `Launching` 与 `Running` 的区别就是"游戏窗口出来没有"：界面的「启动游戏」
/// 按钮在 `Preparing` / `Launching` 期间保持 Loading，到 `Running` 才落回可点状态，
/// 也就是需求里的"直到启动完毕"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchPhase {
    Idle,
    /// 检查、补全缺失文件、解压 natives——还没起进程。
    Preparing,
    /// 进程已经起来，窗口还没出现。
    Launching,
    /// 窗口已出现（或超时后按进程存活判定）。
    Running,
    Exited,
    Failed,
    /// 用户在准备阶段点了「取消启动」。
    ///
    /// 和 `Failed` 分开：取消不是故障，界面不该弹崩溃对话框，也不写崩溃报告。
    Cancelled,
}

impl LaunchPhase {
    /// 界面要不要把启动按钮显示成加载中。
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            LaunchPhase::Preparing | LaunchPhase::Launching | LaunchPhase::Running
        )
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            LaunchPhase::Idle => "未启动",
            LaunchPhase::Preparing => "准备中",
            LaunchPhase::Launching => "启动中",
            LaunchPhase::Running => "运行中",
            LaunchPhase::Exited => "已退出",
            LaunchPhase::Failed => "启动失败",
            LaunchPhase::Cancelled => "已取消",
        }
    }
}

/// 启动流程里的一步（PCL2 式的"分阶段进度"）。
///
/// 顺序固定为 [`LAUNCH_STEP_IDS`]：检查 → 补全 → 解压 → 启动 → 等窗口。
/// 界面照着它画勾，进度条按 [`LAUNCH_STEP_WEIGHTS`] 加权，**只在后端算一次**，
/// 免得前后端两套算法对不上。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchStep {
    pub id: String,
    pub label: String,
    pub status: PhaseStatus,
    /// 这一步正在做什么（文件名、阶段文案）。空串表示没什么可说的。
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub done_bytes: u64,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub files_done: usize,
    #[serde(default)]
    pub files_total: usize,
}

impl LaunchStep {
    pub fn new(id: &str, label: &str) -> Self {
        LaunchStep {
            id: id.to_string(),
            label: label.to_string(),
            status: PhaseStatus::Pending,
            detail: String::new(),
            done_bytes: 0,
            total_bytes: 0,
            files_done: 0,
            files_total: 0,
        }
    }

    /// 这一步自己的完成度（0..=1）。下载步按字节，其余按状态。
    pub fn progress(&self) -> f64 {
        match self.status {
            PhaseStatus::Completed | PhaseStatus::Skipped => 1.0,
            PhaseStatus::Failed => 1.0,
            _ => {
                if self.total_bytes > 0 {
                    (self.done_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
                } else if self.files_total > 0 {
                    (self.files_done as f64 / self.files_total as f64).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
        }
    }
}

/// 五步的 id 与中文名，顺序即执行顺序。
pub const LAUNCH_STEPS: [(&str, &str); 5] = [
    ("check", "检查启动条件"),
    ("repair", "补全缺失文件"),
    ("natives", "解压 natives"),
    ("spawn", "启动游戏进程"),
    ("window", "等待游戏窗口"),
];

/// 每一步在总进度里的权重（加起来 100）。
///
/// 补全文件给了 55：它是唯一真的会花几十秒的一步，权重小了进度条会"卡住不动"。
pub const LAUNCH_STEP_WEIGHTS: [u32; 5] = [8, 55, 7, 15, 15];

/// 按 [`LAUNCH_STEPS`] 建一份全新（全部 pending）的步骤表。
pub fn fresh_launch_steps() -> Vec<LaunchStep> {
    LAUNCH_STEPS
        .iter()
        .map(|(id, label)| LaunchStep::new(id, label))
        .collect()
}

/// 加权总进度（0..=1）。步骤表为空时返回 0。
pub fn launch_progress(steps: &[LaunchStep]) -> f64 {
    if steps.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut weight = 0.0;
    for (index, step) in steps.iter().enumerate() {
        let w = LAUNCH_STEP_WEIGHTS.get(index).copied().unwrap_or(10) as f64;
        weight += w;
        sum += w * step.progress();
    }
    if weight <= 0.0 {
        return 0.0;
    }
    (sum / weight).clamp(0.0, 1.0)
}

/// 一次启动的运行时快照（界面轮询它）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchState {
    /// 本次启动的会话 id（游戏可以多开，界面靠它区分是哪一个进程）。
    ///
    /// `0` 表示"没有会话"（还没启动过）。
    #[serde(default)]
    pub session_id: u64,
    pub phase: LaunchPhase,
    pub root_id: String,
    pub version_id: String,
    /// 游戏进程 pid（还没 spawn 时为 `None`）。
    pub pid: Option<u32>,
    /// 启动时刻（Unix 毫秒）。
    pub started_at: Option<u64>,
    /// 已运行毫秒数（`Exited` 之后是总时长）。
    pub elapsed_ms: u64,
    pub exit_code: Option<i32>,
    /// 本次启动的 console 全量日志。
    pub log_path: Option<String>,
    /// 崩溃前最后若干行（导出时也用这份）。
    pub console_tail_path: Option<String>,
    /// 是否检测到游戏窗口。
    pub game_window_found: bool,
    /// 失败原因（`Failed` 时）。
    pub error: Option<String>,
    /// 这次退出是否判定为崩溃。
    pub crashed: bool,
    /// 启动过程中的说明（GC 降级、自动中文跳过、显卡设置失败……）。
    ///
    /// 这些都不是错误，但"为什么和我设置的不一样"必须能查到，所以直接挂在
    /// 状态上给界面显示，而不是只写日志。
    #[serde(default)]
    pub notes: Vec<String>,
    /// 五个阶段的明细（PCL2 式进度条的数据源）。
    #[serde(default)]
    pub steps: Vec<LaunchStep>,
    /// 加权总进度 0..=1。
    #[serde(default)]
    pub percent: f64,
    /// 当前阶段的一句话说明，界面直接显示在进度条上方。
    #[serde(default)]
    pub stage_label: String,
    /// 这次启动是否被用户取消。
    #[serde(default)]
    pub cancelled: bool,
    /// 补全缺失文件时用的下载任务 id（下载页能看到同一个任务）。
    #[serde(default)]
    pub queue_job_id: Option<String>,
}

impl LaunchState {
    pub fn idle() -> Self {
        LaunchState {
            session_id: 0,
            phase: LaunchPhase::Idle,
            root_id: String::new(),
            version_id: String::new(),
            pid: None,
            started_at: None,
            elapsed_ms: 0,
            exit_code: None,
            log_path: None,
            console_tail_path: None,
            game_window_found: false,
            error: None,
            crashed: false,
            notes: Vec::new(),
            steps: Vec::new(),
            percent: 0.0,
            stage_label: String::new(),
            cancelled: false,
            queue_job_id: None,
        }
    }
}

/// 命中的崩溃关键词。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashKeyword {
    /// 规则 id，如 `mixin_apply_failed`。
    pub id: String,
    /// 中文标签。
    pub label: String,
    /// 命中次数。
    pub count: usize,
}

/// 疑似模组的置信度。界面统一标注「推测 / 可能」，不宣称确定性归因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspectConfidence {
    Low,
    Medium,
    High,
}

impl SuspectConfidence {
    pub fn label_zh(self) -> &'static str {
        match self {
            SuspectConfidence::High => "高度可能",
            SuspectConfidence::Medium => "可能",
            SuspectConfidence::Low => "推测",
        }
    }
}

/// 一个疑似引发崩溃的模组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashSuspect {
    pub file_name: String,
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub confidence: SuspectConfidence,
    /// 证据：命中的那几行日志（截断到合理长度）。
    pub evidence: String,
}

/// 崩溃分析结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashReport {
    pub detected: bool,
    pub exit_code: Option<i32>,
    /// 最新 `crash-reports/crash-*.txt` 的路径。
    pub crash_file: Option<String>,
    pub keywords: Vec<CrashKeyword>,
    pub suspects: Vec<CrashSuspect>,
    /// 崩溃前的日志尾巴（界面预览用，导出里是完整文件）。
    pub log_tail: String,
    pub captured_at: u64,
}

impl CrashReport {
    pub fn none() -> Self {
        CrashReport {
            detected: false,
            exit_code: None,
            crash_file: None,
            keywords: Vec::new(),
            suspects: Vec::new(),
            log_tail: String::new(),
            captured_at: 0,
        }
    }
}

/// 导出结果：目录 + 实际写出的文件名清单。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashExport {
    pub dir: String,
    pub files: Vec<String>,
    pub manifest_path: String,
}

/// 打成 ZIP 的排障包（`crash_export_zip`）。
///
/// 和 [`CrashExport`] 的内容一样，只是落成一个文件：用户自己选目录，
/// 文件名是 `崩溃日志-2026_10_31 20-45-01.zip`（本地时间，冒号换连字符）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashArchive {
    pub path: String,
    pub files: usize,
    pub bytes: u64,
    /// 文件名本身（界面 toast 里显示，省得再切一次路径）。
    pub file_name: String,
}

/// 启动前的检查结果（`launch_check`）。
///
/// 界面拿它做两件事：`ok == false` 时**不启动**并逐条显示 `blocks`；
/// `notes` 是"能启动但你需要知道"的说明（比如自动中文被跳过了）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LaunchCheck {
    pub ok: bool,
    pub blocks: Vec<LaunchBlock>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl LaunchCheck {
    pub fn pass(notes: Vec<String>) -> Self {
        LaunchCheck {
            ok: true,
            blocks: Vec::new(),
            notes,
        }
    }

    pub fn blocked(block: LaunchBlock) -> Self {
        LaunchCheck {
            ok: false,
            blocks: vec![block],
            notes: Vec::new(),
        }
    }
}

/// 挡住启动的一条原因。`kind` 决定界面给什么快捷操作（重新登录 / 打开设置）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchBlock {
    pub kind: LaunchBlockKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchBlockKind {
    /// 一个账号都没有。
    NoAccount,
    /// 微软账号的令牌取不到/被拒。
    Auth,
    /// 找不到可用的 Java。
    Java,
    /// 版本文件不完整（缺 jar / 依赖库 / 资源索引）。
    Files,
    /// 版本本身不可用（JSON 读不出来等）。
    Version,
}

impl LaunchBlockKind {
    /// 界面是否该给一个「重新登录」入口。
    pub fn wants_relogin(self) -> bool {
        matches!(self, LaunchBlockKind::NoAccount | LaunchBlockKind::Auth)
    }
}
