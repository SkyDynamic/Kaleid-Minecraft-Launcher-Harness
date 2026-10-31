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
        needles: &["registry is already frozen", "registryaccess", "registries are frozen"],
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
        needles: &[
            "opengl",
            "glfw error",
            "pixel format",
            "wglmakectx",
            "failed to create window",
        ],
    },
];

/// 判定顺序：命中越具体的规则，置信度越高。
fn confidence_for(rule_id: &str) -> SuspectConfidence {
    match rule_id {
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

    // 崩溃判定：非零退出码，或者日志里出现崩溃报告落盘的提示。
    let fatal_markers = [
        "crash report saved to",
        "a fatal error has been detected",
        "exception in thread \"main\"",
        "uncaught exception",
        "---- minecraft crash report ----",
    ];
    let suspect_signal = fatal_markers.iter().any(|m| lower.contains(m))
        || !keywords.is_empty();
    let detected = exit_code.map(|c| c != 0).unwrap_or(false) || suspect_signal;

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

    #[test]
    fn crash_report_file_marks_detected_even_with_zero_exit() {
        let log = "---- Minecraft Crash Report ----\nCrash report saved to crash-reports/crash-1.txt";
        let report = analyze(log, Some(0), &[], Some("crash-reports/crash-1.txt".into()));
        assert!(report.detected);
        assert_eq!(report.crash_file.as_deref(), Some("crash-reports/crash-1.txt"));
    }
}
