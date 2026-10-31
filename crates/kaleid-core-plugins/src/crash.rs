//! 崩溃判定与关键词分析。
//!
//! 这里的结论**一律是可错的推测**：Minecraft 的崩溃现场（尤其带几十个模组时）
//! 没有任何字段能权威证明"就是某个模组干的"。所以：
//! - 关键词只报"命中了什么、命中几次"，不直接下结论；
//! - 疑似模组给置信度（高度可能 / 可能 / 推测），界面必须照抄这个措辞；
//! - 证据保留原始日志行，用户能自己核对。
//!
//! 规则表是数据，不是分支森林：加一条规则只需要往 [`KEYWORD_RULES`] 里加一行。

use kaleid_api::{CrashKeyword, CrashReport, CrashSuspect, SuspectConfidence};

/// 一条关键词规则：id、中文标签、匹配片段（不区分大小写）。
struct Rule {
    id: &'static str,
    label: &'static str,
    needles: &'static [&'static str],
}

const KEYWORD_RULES: &[Rule] = &[
    Rule {
        id: "oom",
        label: "内存不足",
        needles: &[
            "java.lang.outofmemoryerror",
            "out of memory",
            "unable to allocate",
            "gc overhead limit exceeded",
            "insufficient memory",
        ],
    },
    Rule {
        id: "mod_dependency",
        label: "模组依赖缺失或版本不符",
        needles: &[
            "requires version",
            "mod resolution failed",
            "incompatible mod set",
            "dependency resolution",
            "requires any version",
            "is missing or unsupported",
            "missing dependencies",
        ],
    },
    Rule {
        id: "mixin_apply_failed",
        label: "Mixin 注入失败",
        needles: &[
            "mixin apply failed",
            "mixin apply",
            "mixin transformation of",
            "critical injection failure",
            "failed to apply mixin",
        ],
    },
    Rule {
        id: "class_not_found",
        label: "找不到类（通常是缺少依赖库）",
        needles: &[
            "java.lang.classnotfoundexception",
            "noclassdeffounderror",
            "classnotfoundexception",
        ],
    },
    Rule {
        id: "no_such_method",
        label: "找不到方法/字段（模组与游戏版本不匹配）",
        needles: &[
            "nosuchmethoderror",
            "nosuchfielderror",
            "java.lang.nosuchmethoderror",
        ],
    },
    Rule {
        id: "illegal_access",
        label: "访问权限错误",
        needles: &["illegalaccesserror", "java.lang.illegalaccesserror"],
    },
    Rule {
        id: "duplicate_mods",
        label: "重复的模组",
        needles: &["duplicate mod", "found duplicate", "duplicate mods"],
    },
    Rule {
        id: "registry_freeze",
        label: "注册表被冻结后仍被修改",
        // `registryaccess` 是类名，正常日志和堆栈里都常见，不能当线索。
        needles: &["registry is already frozen", "registries are frozen"],
    },
    Rule {
        id: "jvm_crash",
        label: "JVM 级崩溃（不是模组问题）",
        needles: &[
            "a fatal error has been detected by the java runtime environment",
            "hs_err_pid",
            "sigsegv",
            "internal error (",
        ],
    },
    Rule {
        id: "mod_conflict",
        label: "模组之间冲突",
        needles: &["conflicting", "conflict with", "cannot be loaded together"],
    },
    Rule {
        id: "loader_version",
        label: "加载器版本过旧或过新",
        needles: &[
            "please update fabric loader",
            "unsupported class file major version",
            "requires a newer version of fabric",
            "loader version",
        ],
    },
    Rule {
        id: "graphics",
        label: "显卡/OpenGL 问题",
        // 注意：绝不能收 `opengl` 这个词本身。正常开局会打
        // `OpenGL Vendor/Renderer/Version` 好几行，那样每局都"崩溃"。
        // 这里只收真正的失败措辞。
        needles: &[
            "glfw error",
            "pixel format",
            "wglmakectx",
            "failed to create window",
            "opengl error",
            "opengl exception",
            "glcaps",
        ],
    },
];

/// 日志里出现这些就说明游戏已经在正常关服了（不是崩溃）。
///
/// `Stopping!` 是 Minecraft 客户端退出时必打的一行（1.13+ 起一直存在，
/// 关窗退出、菜单里退出都会打）；后两条是 1.20+ 存档刷盘的收尾日志。
const SHUTDOWN_MARKERS: &[&str] = &[
    "stopping!",
    "saving all world maps",
    "save queue flushed",
];

/// 判定顺序：命中越具体的规则，置信度越高。
fn confidence_for(rule_id: &str) -> SuspectConfidence {    match rule_id {
        "mod_dependency" | "mixin_apply_failed" | "duplicate_mods" | "mod_conflict" => {
            SuspectConfidence::High
        }
        "class_not_found" | "no_such_method" | "illegal_access" | "registry_freeze" => {
            SuspectConfidence::Medium
        }
        _ => SuspectConfidence::Low,
    }
}

/// 分析一段控制台日志，产出崩溃报告。
///
/// - `exit_code`：`None` 表示进程还在跑（理论上不会走到这里）。
/// - `mod_files`：`mods/` 目录里的 jar 文件名，用来把日志里的名字对回具体文件。
/// - `crash_file`：`crash-reports/` 里最新的那个文件（如有）。
pub fn analyze(
    log: &str,
    exit_code: Option<i32>,
    mod_files: &[String],
    crash_file: Option<String>,
) -> CrashReport {
    let lower = log.to_ascii_lowercase();
    let mut keywords = Vec::new();
    for rule in KEYWORD_RULES {
        let mut count = 0;
        for needle in rule.needles {
            count += lower.matches(needle).count();
        }
        if count > 0 {
            keywords.push(CrashKeyword {
                id: rule.id.to_string(),
                label: rule.label.to_string(),
                count,
            });
        }
    }

    // 崩溃判定只看证据，**不看关键词**：关键词是"哪一类问题"的线索，
    // 正常日志里也会出现（OpenGL、模组列表里的 conflicting 之类），
    // 拿它当结论就等于每局都崩。
    //
    // 证据只有三种：
    // 1. 这次会话真的落下了一份崩溃报告（调用方已按会话起始时间过滤过）；
    // 2. 日志里有致命错误标记；
    // 3. 非零退出码，**且**没有走正常关服流程（打出 Stopping! 之类）。
    //    已经关服了却拿到非零码，那是退出码本身的怪癖，不是崩溃。
    let fatal_markers = [
        "crash report saved to",
        "a fatal error has been detected",
        "exception in thread \"main\"",
        "uncaught exception",
        "---- minecraft crash report ----",
    ];
    let fatal = fatal_markers.iter().any(|m| lower.contains(m));
    let shutdown_seen = SHUTDOWN_MARKERS.iter().any(|m| lower.contains(m));
    let bad_exit = matches!(exit_code, Some(code) if code != 0) && !shutdown_seen;
    let detected = fatal || crash_file.is_some() || bad_exit;

    let suspects = find_suspects(log, mod_files, &keywords);
    CrashReport {
        detected,
        exit_code,
        crash_file,
        keywords,
        suspects,
        log_tail: tail_lines(log, 120),
        captured_at: unix_ms(),
    }
}

/// 从日志里找出"很可能有问题"的模组。
///
/// 三条证据链，置信度依次降低：
/// 1. 日志行里同时出现某个 jar 文件名（或它的 modid）与异常关键字 → 可能；
/// 2. 异常堆栈的 `at ...` 行里出现模组的包名前缀 → 推测；
/// 3. 只有"哪个模组加载失败"这种模块级提示 → 高度可能（由规则置信度决定）。
fn find_suspects(log: &str, mod_files: &[String], keywords: &[CrashKeyword]) -> Vec<CrashSuspect> {
    let mut suspects: Vec<CrashSuspect> = Vec::new();
    let exception_words = [
        "exception",
        "error",
        "failed",
        "conflict",
        "missing",
        "incompatible",
        "crash",
    ];

    for file in mod_files {
        let stem = file
            .trim_end_matches(".jar")
            .trim_end_matches(".JAR")
            .to_string();
        // 去掉版本号再匹配：日志里通常只写 `sodium` 或 `sodium-0.5.8.jar`。
        let base = stem
            .split(['-', '_', '+'])
            .next()
            .unwrap_or(&stem)
            .to_ascii_lowercase();
        if base.len() < 3 {
            continue;
        }
        let mut evidence: Vec<&str> = Vec::new();
        let mut strong = false;
        for line in log.lines() {
            let line_lower = line.to_ascii_lowercase();
            if line_lower.contains(&base) || line_lower.contains(&stem.to_ascii_lowercase()) {
                if exception_words.iter().any(|w| line_lower.contains(w)) {
                    strong = true;
                    evidence.push(line);
                } else if evidence.len() < 3 && line_lower.contains("at ") {
                    evidence.push(line);
                }
            }
            if evidence.len() >= 4 {
                break;
            }
        }
        if evidence.is_empty() {
            continue;
        }
        let confidence = if strong {
            SuspectConfidence::Medium
        } else {
            SuspectConfidence::Low
        };
        suspects.push(CrashSuspect {
            file_name: file.clone(),
            mod_id: None,
            name: Some(stem),
            confidence,
            evidence: evidence
                .iter()
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }

    // 规则本身指向模组时，把整体置信度抬一档：这类规则的证据链最直接。
    if let Some(best) = keywords
        .iter()
        .map(|k| confidence_for(&k.id))
        .max()
    {
        if best >= SuspectConfidence::High {
            for suspect in &mut suspects {
                suspect.confidence = SuspectConfidence::High;
            }
        }
    }
    suspects.sort_by(|a, b| b.confidence.cmp(&a.confidence));
    suspects.truncate(8);
    suspects
}

/// 取日志最后 `n` 行：崩溃预览与导出共用，`game.rs` 导出时也要用。
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
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

    #[test]
    fn detects_oom_and_reports_keyword() {
        let log = "java.lang.OutOfMemoryError: Java heap space\n\tat net.minecraft.client.Main";
        let report = analyze(log, Some(1), &[], None);
        assert!(report.detected);
        assert!(report.keywords.iter().any(|k| k.id == "oom"));
    }

    #[test]
    fn clean_exit_is_not_a_crash() {
        let log = "Stopping! \nGame stopped";
        let report = analyze(log, Some(0), &[], None);
        assert!(!report.detected);
        assert!(report.keywords.is_empty());
    }

    #[test]
    fn finds_suspect_mod_by_name_with_exception() {
        let log = "\
java.lang.NoSuchMethodError: net.example.Foo.bar()V
\tat net.example.mixin.FooMixin.apply(sodium-0.5.8.jar)
Failed to load mod sodium-0.5.8.jar";
        let report = analyze(log, Some(1), &["sodium-0.5.8.jar".into(), "jei-1.20.1.jar".into()], None);
        assert_eq!(report.suspects.len(), 1);
        assert_eq!(report.suspects[0].file_name, "sodium-0.5.8.jar");
        assert!(report.suspects[0].confidence >= SuspectConfidence::Medium);
        assert!(!report.suspects[0].evidence.is_empty());
    }

    #[test]
    fn mixin_failure_rule_lifts_confidence() {
        let log = "Mixin apply failed: mixin.example.json -> mod example-1.0.jar";
        let report = analyze(log, Some(1), &["example-1.0.jar".into()], None);
        assert!(report.keywords.iter().any(|k| k.id == "mixin_apply_failed"));
        assert_eq!(
            report.suspects.first().map(|s| s.confidence),
            Some(SuspectConfidence::High)
        );
    }

    /// 真机上报的问题：正常退出游戏也被当成崩溃。
    ///
    /// 这段日志就是一次正常的开局 + 正常退出（退出码 0）。它里面必然有
    /// `OpenGL Vendor/Renderer/Version` 这几行——关键词规则只该把它们当线索，
    /// 绝不能当成崩溃结论。
    #[test]
    fn a_normal_session_with_opengl_lines_is_not_a_crash() {
        let log = "\
[Render thread/INFO]: Backend library: LWJGL version 3.3.3+5
[Render thread/INFO]: OpenGL Vendor: NVIDIA Corporation
[Render thread/INFO]: OpenGL Renderer: NVIDIA GeForce RTX 4060 Laptop GPU/PCIe/SSE2
[Render thread/INFO]: OpenGL Version: 3.3.0 NVIDIA 560.94
[Render thread/INFO]: Narrator library for x64 successfully loaded
[Render thread/INFO]: Sound engine started
[Render thread/INFO]: Stopping!
[Render thread/INFO]: Saving all world maps
[Render thread/INFO]: Save queue flushed!";
        let report = analyze(log, Some(0), &[], None);
        assert!(
            !report.detected,
            "正常退出不是崩溃，命中的关键词：{:?}",
            report.keywords
        );
        assert!(
            report.keywords.is_empty(),
            "日常日志不该报出崩溃线索：{:?}",
            report.keywords
        );
    }

    /// 关窗退出（退出码 0，日志里没有崩溃报告、没有致命错误）同样不是崩溃。
    #[test]
    fn closing_the_window_normally_is_not_a_crash() {
        let log = "[Render thread/INFO]: OpenGL Version: 3.3.0\n\
                   [Render thread/INFO]: Stopping!";
        assert!(!analyze(log, Some(0), &[], None).detected);
    }

    /// 关键词只是线索：模组列表里出现 `conflicting` 这种词不代表游戏崩了。
    #[test]
    fn keyword_hits_alone_are_not_proof_of_a_crash() {
        let log = "[main/INFO]: Conflicting mod ids were resolved at load time\n\
                   [Render thread/INFO]: Stopping!";
        let report = analyze(log, Some(0), &[], None);
        assert!(!report.detected, "命中了关键词不等于崩溃：{:?}", report.keywords);
    }

    /// 反过来：非零退出码加上没有正常关服日志，就是崩溃。
    #[test]
    fn non_zero_exit_without_a_shutdown_line_is_a_crash() {
        let log = "[Render thread/INFO]: Sound engine started\n\
                   Exception in thread \"main\" java.lang.RuntimeException: boom";
        let report = analyze(log, Some(1), &[], None);
        assert!(report.detected);
    }

    /// 已经走到关服流程了（打出了 Stopping!）却拿到非零退出码：
    /// 那是退出码本身的怪癖，不是崩溃。
    #[test]
    fn shutdown_line_wins_over_a_non_zero_exit_code() {
        let log = "[Render thread/INFO]: Stopping!\n[Render thread/INFO]: Save queue flushed!";
        assert!(!analyze(log, Some(1), &[], None).detected);
    }

    #[test]
    fn crash_report_file_marks_detected_even_with_zero_exit() {
        let log = "---- Minecraft Crash Report ----\nCrash report saved to crash-reports/crash-1.txt";
        let report = analyze(log, Some(0), &[], Some("crash-reports/crash-1.txt".into()));
        assert!(report.detected);
        assert_eq!(report.crash_file.as_deref(), Some("crash-reports/crash-1.txt"));
    }

    /// 真机日志回归：拿这台机器上一次真实启动的 `console.log` 走一遍判定。
    ///
    /// 用法（正常退出时应当 `detected == false`）：
    /// `KALEID_REAL_LOG=<日志路径> cargo test -p kaleid-core-plugins --lib \
    ///   crash::tests::real_log_is_judged_by_evidence -- --ignored --nocapture`
    #[test]
    #[ignore = "读这台机器上的真实启动日志，手动跑"]
    fn real_log_is_judged_by_evidence() {
        let Ok(path) = std::env::var("KALEID_REAL_LOG") else {
            eprintln!("没设 KALEID_REAL_LOG，跳过");
            return;
        };
        let log = std::fs::read_to_string(&path).expect("读不到日志");
        let report = analyze(&log, Some(0), &[], None);
        eprintln!(
            "日志 {} 字节，命中的关键词：{:?}，detected = {}",
            log.len(),
            report
                .keywords
                .iter()
                .map(|k| format!("{}({})", k.id, k.count))
                .collect::<Vec<_>>(),
            report.detected
        );
        assert!(!report.detected, "真机正常退出的日志不该判成崩溃");
    }
}
