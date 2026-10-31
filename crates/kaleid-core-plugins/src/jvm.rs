//! JVM 参数：GC 档位，以及"用户的参数和档位怎么并存"的规则。
//!
//! 三条原则：
//! 1. **GC 档位只在用户没自己指定 `-XX:+Use*GC` 时才加**，用户写了就让位。
//! 2. **GC 档位按 Java 版本降级**，并且降级原因要能被用户看到（不是默默换掉）。
//! 3. 这里的输出是"要追加到命令行里的参数"，不负责拼 `-Xmx` / classpath ——
//!    那些由 [`crate::launch`] 按版本 JSON 处理。
//!
//! 默认那四条参数不在这里注入：它们是 [`kaleid_api::DEFAULT_JVM_ARGS`]，
//! 是设置里 JVM 参数栏的**初始内容**，用户能看到、能改、也能删干净。
//!
//! 调优 G1GC 的取值来自 PCL2（`Meloong-Git/PCL`，`Plain Craft Launcher 2/Modules/
//! Minecraft/ModLaunch.vb`，`LaunchAdvanceGC` 累加器）；标准 G1GC 是 Mojang 在
//! 官方启动器 / 1.17+ 默认 JVM 参数里用的那一组（PCL2 源码里把它标成 `Mojang G1GC`）。

use kaleid_api::{
    ApiError, GC_MODE_G1, GC_MODE_G1_TUNED, GC_MODE_NONE, GC_MODE_ZGC, GC_MODE_ZGC_GENERATIONAL,
};

pub use kaleid_api::{default_jvm_args, DEFAULT_JVM_ARGS};

/// 标准 G1GC（Mojang 那组）。
///
/// `-XX:+UnlockExperimentalVMOptions` **不能省**：`G1NewSizePercent` 在 HotSpot 里是
/// experimental 选项（Java 8 / 17 / 21 / 25 都这样），没解锁直接报
/// `VM option 'G1NewSizePercent' is experimental and must be enabled via
/// -XX:+UnlockExperimentalVMOptions` —— 也就是说游戏根本起不来。Mojang 自己那份
/// 默认参数里也是先解锁再给百分比。放在第一位，读命令行时一眼能看出它管的是谁。
const G1_BASE: &[&str] = &[
    "-XX:+UseG1GC",
    "-XX:+UnlockExperimentalVMOptions",
    "-XX:G1NewSizePercent=20",
    "-XX:G1ReservePercent=20",
    "-XX:MaxGCPauseMillis=50",
];

/// 调优 G1GC 在标准基础上多出来的部分（PCL2 `ModLaunch.vb` 的累加器）。
const G1_TUNED_EXTRA: &[&str] = &["-XX:G1HeapRegionSize=32M", "-XX:+PerfDisableSharedMem"];

/// ZGC 需要 Java 15+；分代 ZGC 需要 Java 21+（`ZGenerational` 是 21 引入的，
/// 22 起成为默认但仍可显式指定）。
const ZGC_MIN_JAVA: u32 = 15;
const ZGC_GENERATIONAL_MIN_JAVA: u32 = 21;
/// Java 23 起分代 ZGC 变成唯一/默认模式，`-XX:+ZGenerational` 不必再写。
const ZGC_GENERATIONAL_DEFAULT_SINCE: u32 = 23;
/// Windows 上 ZGC 依赖的大页支持要 1809（build 17763）之后才稳。
const ZGC_MIN_WINDOWS_BUILD: u32 = 17763;

/// 一次 GC 参数计算的结果：要加的参数 + 需要告诉用户的说明。
pub struct GcPlan {
    pub args: Vec<String>,
    pub notes: Vec<String>,
}

/// 按档位算参数。`java_major` 是目标 Java 的主版本号（8 / 17 / 21 …），
/// `windows_build` 只有 Windows 才有值。
pub fn gc_args(mode: &str, java_major: u32, windows_build: Option<u32>) -> GcPlan {
    let mode = mode.trim().to_ascii_lowercase();
    let (mode, mut notes) = match mode.as_str() {
        GC_MODE_ZGC | GC_MODE_ZGC_GENERATIONAL | GC_MODE_G1 | GC_MODE_G1_TUNED | GC_MODE_NONE => {
            (mode, Vec::new())
        }
        other => {
            // 不认识的值不猜：当成默认档，并说明。
            (
                GC_MODE_ZGC_GENERATIONAL.to_string(),
                vec![format!("不认识的 GC 档位「{other}」，按默认的分代 ZGC 处理")],
            )
        }
    };

    if mode == GC_MODE_NONE {
        return GcPlan {
            args: Vec::new(),
            notes,
        };
    }

    let wants_zgc = mode == GC_MODE_ZGC || mode == GC_MODE_ZGC_GENERATIONAL;
    if wants_zgc {
        if let Some(build) = windows_build {
            if build < ZGC_MIN_WINDOWS_BUILD {
                notes.push(format!(
                    "系统版本太旧（Windows build {build} < {ZGC_MIN_WINDOWS_BUILD}），ZGC 换成标准 G1GC"
                ));
                return GcPlan {
                    args: g1_args(GC_MODE_G1),
                    notes,
                };
            }
        }
        if java_major < ZGC_MIN_JAVA {
            notes.push(format!(
                "Java {java_major} 不支持 ZGC（需要 {ZGC_MIN_JAVA}+），换成标准 G1GC"
            ));
            return GcPlan {
                args: g1_args(GC_MODE_G1),
                notes,
            };
        }
        if mode == GC_MODE_ZGC_GENERATIONAL {
            // Java 23 起分代 ZGC 就是默认模式了，再显式写 `-XX:+ZGenerational`
            // 会被当成"过时选项"（Java 24 之后连非分代模式都没了）。
            // 只有 21 / 22 需要这个开关。
            if java_major >= ZGC_GENERATIONAL_DEFAULT_SINCE {
                return GcPlan {
                    args: vec!["-XX:+UseZGC".into()],
                    notes,
                };
            }
            if java_major >= ZGC_GENERATIONAL_MIN_JAVA {
                return GcPlan {
                    args: vec!["-XX:+UseZGC".into(), "-XX:+ZGenerational".into()],
                    notes,
                };
            }
            notes.push(format!(
                "分代 ZGC 需要 Java {ZGC_GENERATIONAL_MIN_JAVA}+（当前 Java {java_major}），用非分代 ZGC"
            ));
        }
        return GcPlan {
            args: vec!["-XX:+UseZGC".into()],
            notes,
        };
    }

    GcPlan {
        args: g1_args(&mode),
        notes,
    }
}

/// G1 的两档（`g1` 标准 / `g1_tuned` 调优）在 Java 版本上的附加项。
fn g1_args(mode: &str) -> Vec<String> {
    let mut args: Vec<String> = G1_BASE.iter().map(|a| a.to_string()).collect();
    if mode == GC_MODE_G1_TUNED {
        args.extend(G1_TUNED_EXTRA.iter().map(|a| a.to_string()));
    }
    args
}

/// [`g1_args`] 里依赖 Java 版本的那些补丁。
///
/// 单独拿出来是因为它们要看 `java_major`，而 PCL2 的矩阵是这样：
/// - Java 7 及更早：`-XX:MaxPermSize=512m`（PermGen 还没被 Metaspace 取代）
/// - Java 8+：`-XX:+ParallelRefProcEnabled`（引用处理并行，减少 G1 停顿）
/// - Java 12+ 且堆大：`-XX:MinHeapFreeRatio` / `-XX:MaxHeapFreeRatio` 收窄，避免 G1 反复扩缩堆
/// - Java 24+：`-XX:+UseCompactObjectHeaders`（JEP 519，省内存）
///
/// 前两条是**兼容性**补丁，标准与调优两档都加；后两条属于"调优"，只给调优档，
/// 这样"选了标准 G1GC 就是 Mojang 那一组"这句话才站得住。
pub fn g1_version_tweaks(mode: &str, java_major: u32, memory_mb: u32) -> Vec<String> {
    if mode == GC_MODE_NONE {
        return Vec::new();
    }
    let tuned = mode == GC_MODE_G1_TUNED;
    let mut args = Vec::new();
    if java_major <= 7 {
        args.push("-XX:MaxPermSize=512m".to_string());
        return args;
    }
    if java_major >= 8 {
        args.push("-XX:+ParallelRefProcEnabled".to_string());
    }
    if tuned && java_major >= 12 && memory_mb >= 4096 {
        args.push("-XX:MinHeapFreeRatio=25".to_string());
        args.push("-XX:MaxHeapFreeRatio=40".to_string());
    }
    if tuned && java_major >= 24 {
        args.push("-XX:+UseCompactObjectHeaders".to_string());
    }
    args
}

/// 校验用户在设置里填的一行参数。
///
/// 只拦真正会破坏命令行的东西（换行 / NUL / 引号不配对），不做"看起来像不像
/// JVM 参数"的判断——那会把合法的新参数挡在外面。
pub fn validate_user_arg(arg: &str) -> Result<(), ApiError> {
    if arg.is_empty() {
        return Err(ApiError::invalid("JVM 参数不能是空行"));
    }
    if arg.contains('\n') || arg.contains('\r') || arg.contains('\0') {
        return Err(ApiError::invalid("JVM 参数不能包含换行或空字符"));
    }
    let quotes = arg.chars().filter(|c| *c == '"').count();
    if quotes % 2 != 0 {
        return Err(ApiError::invalid(format!("JVM 参数里的双引号不成对：{arg}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_args_are_the_documented_four() {
        assert_eq!(DEFAULT_JVM_ARGS.len(), 4);
        assert!(DEFAULT_JVM_ARGS.contains(&"-XX:-OmitStackTraceInFastThrow"));
        assert!(DEFAULT_JVM_ARGS.contains(&"-Djdk.lang.Process.allowAmbiguousCommands=True"));
        assert!(DEFAULT_JVM_ARGS.contains(&"-Dfml.ignoreInvalidMinecraftCertificates=True"));
        assert!(DEFAULT_JVM_ARGS.contains(&"-Dfml.ignorePatchDiscrepancies=True"));
        // 设置里的初始内容与这份清单必须一模一样，否则"输入框里看到的"和
        // "默认给的"就是两回事了
        assert_eq!(default_jvm_args().len(), DEFAULT_JVM_ARGS.len());
    }

    #[test]
    fn zgc_generational_needs_java_21() {
        let plan = gc_args(GC_MODE_ZGC_GENERATIONAL, 17, None);
        assert_eq!(plan.args, vec!["-XX:+UseZGC"]);
        assert!(plan.notes.iter().any(|n| n.contains("21")));

        let plan = gc_args(GC_MODE_ZGC_GENERATIONAL, 21, None);
        assert_eq!(plan.args, vec!["-XX:+UseZGC", "-XX:+ZGenerational"]);
        assert!(plan.notes.is_empty());

        // Java 23+：分代是默认，不再写那个已经过时的开关
        let plan = gc_args(GC_MODE_ZGC_GENERATIONAL, 24, None);
        assert_eq!(plan.args, vec!["-XX:+UseZGC"]);
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn zgc_falls_back_on_old_java_and_old_windows() {
        let plan = gc_args(GC_MODE_ZGC, 8, None);
        assert!(plan.args.iter().any(|a| a == "-XX:+UseG1GC"));
        assert!(plan.notes.iter().any(|n| n.contains("ZGC")));

        let plan = gc_args(GC_MODE_ZGC, 21, Some(17134));
        assert!(plan.args.iter().any(|a| a == "-XX:+UseG1GC"));
        assert!(plan.notes.iter().any(|n| n.contains("17763")));
    }

    #[test]
    fn tuned_g1_matches_the_pcl2_set() {
        let plan = gc_args(GC_MODE_G1_TUNED, 21, None);
        for arg in [
            "-XX:+UseG1GC",
            "-XX:G1NewSizePercent=20",
            "-XX:G1ReservePercent=20",
            "-XX:G1HeapRegionSize=32M",
            "-XX:MaxGCPauseMillis=50",
            "-XX:+PerfDisableSharedMem",
        ] {
            assert!(plan.args.iter().any(|a| a == arg), "缺少 {arg}");
        }
        // 标准档不该带调优那两条
        let plain = gc_args(GC_MODE_G1, 21, None);
        assert!(!plain.args.iter().any(|a| a == "-XX:G1HeapRegionSize=32M"));
        assert!(!plain.args.iter().any(|a| a == "-XX:+PerfDisableSharedMem"));
    }

    #[test]
    fn g1_tweaks_follow_the_java_version_matrix() {
        // Java 8+ 加 ParallelRefProcEnabled（兼容性补丁，两档 G1 都有）
        assert!(g1_version_tweaks(GC_MODE_G1, 8, 4096)
            .iter()
            .any(|a| a == "-XX:+ParallelRefProcEnabled"));
        assert!(g1_version_tweaks(GC_MODE_G1, 17, 4096)
            .iter()
            .any(|a| a == "-XX:+ParallelRefProcEnabled"));
        // 堆太小就不放开 free ratio
        assert!(g1_version_tweaks(GC_MODE_G1_TUNED, 21, 2048)
            .iter()
            .all(|a| a != "-XX:MinHeapFreeRatio=25"));
        assert!(g1_version_tweaks(GC_MODE_G1_TUNED, 21, 8192)
            .iter()
            .any(|a| a == "-XX:MinHeapFreeRatio=25"));
        // Java 7 只有 PermSize
        let old = g1_version_tweaks(GC_MODE_G1_TUNED, 7, 8192);
        assert_eq!(old, vec!["-XX:MaxPermSize=512m"]);
        // Java 24+ 有紧凑对象头
        assert!(g1_version_tweaks(GC_MODE_G1_TUNED, 24, 8192)
            .iter()
            .any(|a| a == "-XX:+UseCompactObjectHeaders"));
    }

    /// 实验性选项必须先解锁，否则 JVM 直接拒绝启动：
    ///
    /// ```text
    /// Error: VM option 'G1NewSizePercent' is experimental and must be enabled
    ///        via -XX:+UnlockExperimentalVMOptions.
    /// ```
    ///
    /// 这是真机上踩到的坑（Java 8 / 17 / 21 / 25 都会这么报），所以这里钉死两条：
    /// 只要出现 `G1NewSizePercent`，就必须同时出现解锁开关，而且解锁要排在它前面。
    #[test]
    fn experimental_flags_are_unlocked_before_use() {
        const UNLOCK: &str = "-XX:+UnlockExperimentalVMOptions";
        const EXPERIMENTAL: &[&str] = &["-XX:G1NewSizePercent=20"];

        for mode in [GC_MODE_G1, GC_MODE_G1_TUNED] {
            for major in [8u32, 17, 21, 25] {
                let plan = gc_args(mode, major, None);
                let mut args = plan.args.clone();
                args.extend(g1_version_tweaks(mode, major, 8192));

                let uses_experimental = args.iter().any(|a| EXPERIMENTAL.contains(&a.as_str()));
                assert!(uses_experimental, "{mode} 的 G1 档位应当带上 G1NewSizePercent");

                let unlock_at = args
                    .iter()
                    .position(|a| a == UNLOCK)
                    .unwrap_or_else(|| panic!("{mode}/{major} 缺少 {UNLOCK}：{args:?}"));
                for flag in EXPERIMENTAL {
                    if let Some(at) = args.iter().position(|a| a == flag) {
                        assert!(
                            unlock_at < at,
                            "{UNLOCK} 必须排在 {flag} 前面：{args:?}"
                        );
                    }
                }
            }
        }
        // 不指定档位时一条都不加（也就不会有"解锁了但没用"的多余参数）
        assert!(gc_args(GC_MODE_NONE, 21, None).args.is_empty());
    }

    /// 真机证据：把生成出来的参数交给这台机器上每个真实 Java 跑一遍。
    ///
    /// 单元测试只能证明"我们拼了什么"，证明不了"JVM 认不认"——这次的
    /// 实验性选项问题就属于后者。找不到任何 Java 就跳过。
    ///
    /// ```powershell
    /// cargo test -p kaleid-core-plugins --lib jvm::tests::generated_args_are_accepted_by_real_java -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "需要本机装有 Java，手动跑"]
    fn generated_args_are_accepted_by_real_java() {
        let candidates = [
            r"C:\Program Files\Java",
            r"C:\Program Files\Eclipse Adoptium",
            r"C:\Program Files\Microsoft",
            r"C:\Program Files\Zulu",
        ];
        let mut javas: Vec<std::path::PathBuf> = Vec::new();
        for base in candidates {
            let Ok(entries) = std::fs::read_dir(base) else {
                continue;
            };
            for entry in entries.flatten() {
                let exe = entry.path().join("bin").join("java.exe");
                if exe.is_file() {
                    javas.push(exe);
                }
            }
        }
        if javas.is_empty() {
            println!("这台机器上没有找到 Java，跳过");
            return;
        }

        let mut checked = 0;
        for java in javas {
            let version = std::process::Command::new(&java)
                .arg("-version")
                .output()
                .expect("跑得起来吧");
            let text = String::from_utf8_lossy(&version.stderr).to_string();
            let Some(major) = crate::launch::parse_java_version(&text) else {
                println!("跳过（认不出版本）：{}", java.display());
                continue;
            };

            for mode in [
                GC_MODE_G1,
                GC_MODE_G1_TUNED,
                GC_MODE_ZGC,
                GC_MODE_ZGC_GENERATIONAL,
            ] {
                let plan = gc_args(mode, major, None);
                let mut args: Vec<String> = plan.args.clone();
                args.extend(g1_version_tweaks(mode, major, 8192));
                let result = std::process::Command::new(&java)
                    .args(&args)
                    // 用最小堆，别真占内存
                    .arg("-Xmx64m")
                    .arg("-Xms16m")
                    .arg("-version")
                    .output()
                    .unwrap();
                let err = String::from_utf8_lossy(&result.stderr);
                assert!(
                    result.status.success(),
                    "Java {major} 跑 {mode} 失败：{args:?}\n{err}"
                );
                assert!(
                    !err.contains("is experimental"),
                    "Java {major} 的 {mode} 里有没解锁的实验性选项：{args:?}"
                );
                checked += 1;
            }
            println!("Java {major}：{} 全部 Ok", java.display());
        }
        assert!(checked > 0);
    }

    #[test]
    fn none_touches_nothing() {
        let plan = gc_args(GC_MODE_NONE, 21, None);
        assert!(plan.args.is_empty());
        assert!(plan.notes.is_empty());
        assert!(g1_version_tweaks(GC_MODE_NONE, 8, 8192).is_empty());
    }

    #[test]
    fn unknown_mode_is_explained_not_swallowed() {
        let plan = gc_args("shenandoah", 21, None);
        assert!(plan.args.iter().any(|a| a == "-XX:+ZGenerational"));
        assert!(plan.notes.iter().any(|n| n.contains("shenandoah")));
    }

    #[test]
    fn user_arg_validation_catches_broken_input_only() {
        assert!(validate_user_arg("-Xmx4G").is_ok());
        assert!(validate_user_arg("-Dfoo=\"bar\"").is_ok());
        assert!(validate_user_arg("-Dfoo=bad\"quote").is_err());
        assert!(validate_user_arg("line\nbreak").is_err());
        assert!(validate_user_arg("").is_err());
    }
}
