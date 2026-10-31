//! 启动参数解析（`LaunchPlannerApi`）。
//!
//! 做的事：读版本 JSON 链 → 合并 `inheritsFrom` → 按 `rules` 过滤库与参数 →
//! 变量替换 → 按隔离策略算 `--gameDir` → 产出可执行命令行。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::gpu;
use crate::jvm;
use crate::versions::VersionStore;
use kaleid_api::{
    Account, AccountKind, ApiError, ApiResult, EffectiveConfig, IsolationMode, JavaServiceApi,
    LaunchPlan, LaunchPlannerApi, Root, RootRegistryService, SettingsServiceApi,
};

pub struct LaunchPlanner {
    settings: Arc<dyn SettingsServiceApi>,
    roots: Arc<dyn RootRegistryService>,
    store: VersionStore,
    /// `java-runtime` 插件被停用时为 `None`：此时只认设置里手动指定的路径。
    java: Option<Arc<dyn JavaServiceApi>>,
}

impl LaunchPlanner {
    pub fn new(settings: Arc<dyn SettingsServiceApi>, roots: Arc<dyn RootRegistryService>) -> Self {
        LaunchPlanner {
            settings,
            roots,
            store: VersionStore::new(),
            java: None,
        }
    }

    /// 接上 Java 发现能力：没手动指定时按版本要求的主版本自动挑一个。
    pub fn with_java(mut self, java: Option<Arc<dyn JavaServiceApi>>) -> Self {
        self.java = java;
        self
    }

    /// 带上文件夹上下文（能正确取到版本级设置与隔离策略）。
    pub fn plan_for_root(
        &self,
        root: &Root,
        version_id: &str,
        account: &Account,
    ) -> ApiResult<LaunchPlan> {
        let effective = self
            .settings
            .resolve(&root.id, version_id, root.isolation)?;
        build_plan(
            &self.store,
            Path::new(&root.path),
            version_id,
            account,
            &effective,
            self.java.as_deref(),
        )
    }
}

impl LaunchPlannerApi for LaunchPlanner {
    fn plan(&self, root_path: &Path, version_id: &str, account: &Account) -> ApiResult<LaunchPlan> {
        let roots = self.roots.list()?;
        let root = roots
            .into_iter()
            .find(|r| same_path(&r.path, &root_path.to_string_lossy()));
        match root {
            Some(root) => self.plan_for_root(&root, version_id, account),
            None => {
                let effective =
                    self.settings
                        .resolve("adhoc", version_id, IsolationMode::Inherit)?;
                build_plan(
                    &self.store,
                    root_path,
                    version_id,
                    account,
                    &effective,
                    self.java.as_deref(),
                )
            }
        }
    }
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// 拼 classpath 与"缺不缺文件"的检查共用同一套库路径推导，见
/// [`crate::install::library_relative_path`] / [`crate::install::library_path_from_name`]。
use crate::install::{artifact_key, library_relative_path};

// ---------------------------------------------------------------------------
// rules
// ---------------------------------------------------------------------------
/// 判定一条 `rules` 数组是否允许当前环境。遵循 MC 的"按顺序覆盖"语义。
///
/// 完整性检查也复用它（不然会把别的系统的 natives 当成"缺文件"）。
pub(crate) fn rules_allow(rules: &[Value]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule) {
            allowed = rule.get("action").and_then(|a| a.as_str()) == Some("allow");
        }
    }
    allowed
}

fn rule_matches(rule: &Value) -> bool {
    if let Some(os) = rule.get("os") {
        if let Some(name) = os.get("name").and_then(|v| v.as_str()) {
            let current = match std::env::consts::OS {
                "windows" => "windows",
                "macos" => "osx",
                other => other,
            };
            // MC 用 `osx` 表示 macOS
            let want = if name == "osx" { "osx" } else { name };
            if current != want {
                return false;
            }
        }
        if let Some(arch) = os.get("arch").and_then(|v| v.as_str()) {
            let current = std::env::consts::ARCH;
            let matches = match arch {
                "x86" => current == "x86",
                other => current == other,
            };
            if !matches {
                return false;
            }
        }
    }
    if let Some(features) = rule.get("features").and_then(|v| v.as_object()) {
        // 我们不启用任何可选特性（has_custom_resolution / is_demo_user 等）
        for (_, want) in features {
            if want.as_bool() == Some(true) {
                return false;
            }
        }
    }
    true
}

/// 展开 `arguments.jvm` / `arguments.game` 里的一项（可能是字符串或带 rules 的对象）。
fn expand_arg(entry: &Value, out: &mut Vec<String>) {
    match entry {
        Value::String(s) => out.push(s.clone()),
        Value::Object(_) => {
            let allowed = entry
                .get("rules")
                .and_then(|r| r.as_array())
                .map(|r| rules_allow(r))
                .unwrap_or(true);
            if !allowed {
                return;
            }
            match entry.get("value") {
                Some(Value::String(s)) => out.push(s.clone()),
                Some(Value::Array(items)) => {
                    for item in items {
                        if let Some(s) = item.as_str() {
                            out.push(s.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 组装
// ---------------------------------------------------------------------------

fn build_plan(
    store: &VersionStore,
    root: &Path,
    version_id: &str,
    account: &Account,
    effective: &EffectiveConfig,
    java: Option<&dyn JavaServiceApi>,
) -> ApiResult<LaunchPlan> {
    let chain = store.load_chain(root, version_id)?;

    // 1) 标量字段：子覆盖父
    let main_class = chain
        .iter()
        .filter_map(|v| v.get("mainClass").and_then(|m| m.as_str()))
        .next_back()
        .ok_or_else(|| ApiError::Format(format!("版本 {version_id} 缺少 mainClass")))?
        .to_string();

    let asset_index = crate::versions::asset_index_id(&chain);
    // 游戏内标识：`--versionType` 是 MC 唯一会把"启动器自述"画到标题界面左下角
    // 版本行上的入口（PCL 也用它）。`None` = 用户关掉了标识，稍后要把这对参数整对摘掉。
    let launcher_brand = effective.launch_brand.clone();

    // 2) 库：按「group:artifact[:classifier]」身份去重（忽略版本号），后出现的覆盖
    //    先出现的。
    //
    //    这一条是硬要求，不是优化：官方的原版 JSON 里已经有 `org.ow2.asm:asm:9.6`，
    //    加载器 profile 又会带一份 `asm:9.9`，两份都进 classpath 的话 Fabric 直接
    //    拒绝启动（`duplicate ASM classes found on classpath`）。HMCL / PCL 也都是
    //    按同一个身份键去重的。
    let mut libraries: BTreeMap<String, Value> = BTreeMap::new();
    for value in &chain {
        let Some(libs) = value.get("libraries").and_then(|v| v.as_array()) else {
            continue;
        };
        for lib in libs {
            let allowed = lib
                .get("rules")
                .and_then(|r| r.as_array())
                .map(|r| rules_allow(r))
                .unwrap_or(true);
            if !allowed {
                continue;
            }
            let name = lib
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }
            libraries.insert(artifact_key(&name), lib.clone());
        }
    }

    let libraries_dir = root.join("libraries");
    // 自包含安装把加载器的库放在 `versions/<id>/libraries/`，原版共享依赖仍在根。
    // 查找顺序：版本本地优先，根兜底（老的分离布局两种都只会在根里）。
    let version_libraries_dir = root.join("versions").join(version_id).join("libraries");
    let mut classpath: Vec<String> = Vec::new();
    for lib in libraries.values() {
        // 优先用 JSON 里写好的路径；很多第三方启动器（PCL / HMCL）产出的加载器
        // profile 只给 maven 坐标 + `url`，**没有** `downloads.artifact`。
        // 以前这里直接 continue，结果 fabric-loader / intermediary / sponge-mixin
        // 全被丢掉，`net.fabricmc.loader.impl.launch.knot.KnotClient` 找不到，
        // JVM 起来 200 毫秒就以 exit code 1 退出，日志里连一行 MC 的都没有。
        let path = library_relative_path(lib);
        let Some(path) = path else {
            continue;
        };
        let local = version_libraries_dir.join(&path);
        let chosen = if local.is_file() {
            local
        } else {
            libraries_dir.join(&path)
        };
        classpath.push(chosen.to_string_lossy().to_string());
    }

    // 3) 客户端 jar：自包含版本目录里就是 `<id>.jar`；否则顺着链条找声明
    //    downloads.client 的那一层（老的分离布局）。
    let own_jar = root
        .join("versions")
        .join(version_id)
        .join(format!("{version_id}.jar"));
    let client_jar = if own_jar.is_file() {
        own_jar
    } else {
        let client_version = chain
            .iter()
            .rev()
            .find(|v| v.get("downloads").and_then(|d| d.get("client")).is_some())
            .and_then(|v| v.get("id").and_then(|i| i.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| version_id.to_string());
        root.join("versions")
            .join(&client_version)
            .join(format!("{client_version}.jar"))
    };
    classpath.push(client_jar.to_string_lossy().to_string());

    // 4) 隔离决定 gameDir
    let game_dir = if effective.isolated {
        root.join("versions").join(version_id)
    } else {
        root.to_path_buf()
    };
    let games_dir_str = games_dir_string(&game_dir);
    let natives_dir = game_dir.join("natives");
    let assets_dir = root.join("assets");

    // 5) 变量表
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert("auth_player_name".into(), account.name.clone());
    vars.insert("auth_uuid".into(), account.uuid.replace('-', ""));
    vars.insert(
        "auth_access_token".into(),
        if account.kind == AccountKind::Offline {
            "0".into()
        } else {
            // 令牌运行时注入，绝不进计划快照（计划会被日志、界面、甚至用户看到）：
            // 真正 spawn 时用 `MsaAuthApi::access_token(account.id)` 取，取不到就
            // 说明需要重新登录，而不是拿 "0" 去连服务器然后被踢。
            "0".into()
        },
    );
    vars.insert(
        "user_type".into(),
        match account.kind {
            AccountKind::Msa => "msa".into(),
            AccountKind::Offline => "legacy".into(),
        },
    );
    vars.insert("auth_xuid".into(), String::new());
    // `${clientid}` 是官方启动器自己的客户端标识（Realms / 遥测用），官方 JSON 里
    // 1.20+ 的 `--clientId ${clientid}` 必须有个值——不给就会把占位符原样写进命令行。
    // 我们没有 Realms 客户端，按官方默认值给空串（见上面「空串也是合法取值」）。
    vars.insert("clientid".into(), String::new());
    vars.insert("version_name".into(), version_id.to_string());
    let game_version =
        crate::versions::base_game_version(&chain, version_id).unwrap_or_else(|| version_id.to_string());
    vars.insert(
        "version_type".into(),
        launcher_brand.clone().unwrap_or_default(),
    );
    vars.insert("game_directory".into(), games_dir_str.clone());
    vars.insert(
        "game_assets".into(),
        assets_dir.to_string_lossy().to_string(),
    );
    vars.insert(
        "assets_root".into(),
        assets_dir.to_string_lossy().to_string(),
    );
    vars.insert(
        "assets_index_name".into(),
        asset_index.clone().unwrap_or_else(|| "legacy".into()),
    );
    vars.insert(
        "natives_directory".into(),
        natives_dir.to_string_lossy().to_string(),
    );
    vars.insert(
        "library_directory".into(),
        if version_libraries_dir.is_dir() {
            version_libraries_dir.to_string_lossy().to_string()
        } else {
            libraries_dir.to_string_lossy().to_string()
        },
    );
    vars.insert("classpath".into(), classpath.join(";"));
    vars.insert("classpath_separator".into(), ";".into());
    vars.insert("launcher_name".into(), "Kaleid".into());
    vars.insert("launcher_version".into(), "0.1.0".into());
    let (w, h) = parse_window(&effective.window);
    vars.insert("resolution_width".into(), w);
    vars.insert("resolution_height".into(), h);

    // 6) 参数展开
    let mut jvm_raw: Vec<String> = Vec::new();
    let mut game_raw: Vec<String> = Vec::new();
    let mut legacy_args: Option<String> = None;
    for value in &chain {
        if let Some(args) = value.get("arguments") {
            if let Some(list) = args.get("jvm").and_then(|v| v.as_array()) {
                for entry in list {
                    expand_arg(entry, &mut jvm_raw);
                }
            }
            if let Some(list) = args.get("game").and_then(|v| v.as_array()) {
                for entry in list {
                    expand_arg(entry, &mut game_raw);
                }
            }
        }
        if let Some(legacy) = value.get("minecraftArguments").and_then(|v| v.as_str()) {
            legacy_args = Some(legacy.to_string());
        }
    }
    if game_raw.is_empty() {
        if let Some(legacy) = legacy_args {
            game_raw = legacy.split_whitespace().map(|s| s.to_string()).collect();
        }
    }

    // `-cp ${classpath}` 由我们自己拼，先从版本自带 jvm 参数里摘掉，避免重复
    let mut jvm_args: Vec<String> = Vec::new();
    let mut skip_next = false;
    for token in jvm_raw {
        if skip_next {
            skip_next = false;
            continue;
        }
        if token == "-cp" || token == "-classpath" {
            skip_next = true;
            continue;
        }
        if token == "${classpath}" {
            continue;
        }
        jvm_args.push(substitute(&token, &vars));
    }

    // 空串也是**合法取值**：`--xuid ""` / `--clientId ""` 必须成对保留。一旦把这个
    // 空 token 过滤掉，`--xuid` 就会把下一个参数（`--versionType`）当成自己的值，
    // 整条命令行错位、游戏秒退且一行日志都不留。真正需要整对摘掉的是「关掉标识」
    // 那种情况，由 `strip_version_type` 专门处理。
    let mut game_args: Vec<String> = game_raw.iter().map(|t| substitute(t, &vars)).collect();
    if launcher_brand.is_none() {
        strip_version_type(&mut game_args);
    }

    let brand_jvm = launcher_brand.clone().unwrap_or_else(|| "Kaleid".to_string());
    let mut extra_jvm_args = vec![
        format!("-Xmx{}M", effective.memory_mb),
        format!("-Xms{}M", (effective.memory_mb / 2).max(512)),
        format!("-Djava.library.path={}", natives_dir.to_string_lossy()),
        format!("-Dminecraft.launcher.brand={brand_jvm}"),
        "-Dminecraft.launcher.version=0.1.0".to_string(),
    ];

    // 6) 用哪个 Java：手动指定 > 按版本要求的主版本自动匹配 > PATH 里的 java
    let java_major_declared = chain
        .iter()
        .filter_map(|v| {
            v.get("javaVersion")
                .and_then(|j| j.get("majorVersion"))
                .and_then(|m| m.as_u64())
        })
        .next_back()
        .map(|m| m as u32);
    let (java_path, java_source) = resolve_java(effective, java, java_major_declared);
    // GC 档位要按**实际会用的那个 Java** 算，而不是版本 JSON 里写的要求：
    // 用户手动指定了 Java 8 去开 1.21，ZGC 就是跑不起来。
    let java_major = java_major_of(&java_path).or(java_major_declared).unwrap_or(8);

    // 7) JVM 参数：GC（含版本补丁）→ 用户参数
    //
    // 那四条默认参数（`-XX:-OmitStackTraceInFastThrow` 等）**不在这里注入**：
    // 它们是设置里 JVM 参数栏的初始内容，已经在 `effective.jvm_args` 里了，
    // 用户删掉就该真的没有。这也是"输入框里的内容 = 命令行里的内容"这个承诺。
    //
    // 顺序仍然是有意的：越靠后越"用户说了算"。GC 档位排在用户参数前，
    // 所以用户手写 `-XX:+Use*GC` 时内置档位整体让位（JVM 取最后一个生效）。
    let user_args = effective.jvm_args.clone();
    let mut jvm_notes: Vec<String> = Vec::new();

    let gc = jvm::gc_args(&effective.gc_mode, java_major, gpu::windows_build());
    let gc_user_override = user_args.iter().any(|a| a.starts_with("-XX:+Use"));
    if gc_user_override {
        jvm_notes.push("你指定了自己的 GC 开关，所以没有套用内置的 GC 档位".to_string());
    } else {
        extra_jvm_args.extend(gc.args.iter().cloned());
        extra_jvm_args.extend(jvm::g1_version_tweaks(
            &effective.gc_mode,
            java_major,
            effective.memory_mb,
        ));
    }
    jvm_notes.extend(gc.notes);

    extra_jvm_args.extend(user_args);

    Ok(LaunchPlan {
        version_id: version_id.to_string(),
        java_path,
        java_source,
        game_dir: games_dir_str.clone(),
        mc_dir: root.to_string_lossy().to_string(),
        assets_dir: assets_dir.to_string_lossy().to_string(),
        libraries_dir: libraries_dir.to_string_lossy().to_string(),
        isolated: effective.isolated,
        main_class,
        classpath,
        jvm_args,
        game_args,
        extra_jvm_args,
        username: account.name.clone(),
        uuid: account.uuid.clone(),
        asset_index,
        launcher_brand,
        launch_dir: games_dir_str.clone(),
        natives_dir: natives_dir.to_string_lossy().to_string(),
        libraries_dirs: if version_libraries_dir.is_dir() {
            vec![
                version_libraries_dir.to_string_lossy().to_string(),
                libraries_dir.to_string_lossy().to_string(),
            ]
        } else {
            vec![libraries_dir.to_string_lossy().to_string()]
        },
        game_version,
        access_token_injected: false,
        jvm_notes,
        auto_chinese: effective.auto_chinese,
        write_latest_launch_bat: effective.write_latest_launch_bat,
        high_performance_gpu: effective.high_performance_gpu,
    })
}

/// 从 java 可执行文件问出主版本号（`java -version` 写 stderr）。
///
/// 问不出来就返回 `None`，调用方回落到版本 JSON 声明的要求——宁可按声明的算，
/// 也不要因为一次探测失败就把参数全丢掉。
fn java_major_of(java_path: &str) -> Option<u32> {
    let output = std::process::Command::new(java_path)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_java_version(&text)
}

/// 从 `java -version` 的输出里解析主版本号。
///
/// 形如：
/// - `java version "1.8.0_402"` → 8
/// - `openjdk version "17.0.10" 2024-01-16` → 17
/// - `openjdk version "21" 2023-09-19` → 21
pub fn parse_java_version(text: &str) -> Option<u32> {
    let start = text.find('"')? + 1;
    let rest = &text[start..];
    let end = rest.find('"')?;
    let version = &rest[..end];
    let mut parts = version.split(['.', '_', '-']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        // 1.8.0_402 → 8
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// 从参数表里摘掉 `--versionType <值>` / `--versionType=<值>`。
///
/// MC 在各版本里这两种写法都出现过（现代 JSON 是两个 token，老的
/// `minecraftArguments` 里也是两个 token），所以两种都处理。
fn strip_version_type(args: &mut Vec<String>) {
    let drained: Vec<String> = args.drain(..).collect();
    let mut out: Vec<String> = Vec::with_capacity(drained.len());
    let mut iter = drained.into_iter();
    while let Some(token) = iter.next() {
        if token == "--versionType" {
            // 后面那个 token 是这个参数的值，一起丢掉。
            iter.next();
            continue;
        }
        if token.starts_with("--versionType=") {
            continue;
        }
        out.push(token);
    }
    *args = out;
}

/// 决定本次启动用哪个 Java，并说明理由。
///
/// 手动指定的路径原样使用（哪怕它不存在——那是用户写错了，界面上会标红，
/// 而不是被我们悄悄换成一个"看起来能用"的）。找不到任何 Java 时退回
/// `java`，交给系统 PATH，`java_source` 会显示成"回退"。
fn resolve_java(
    effective: &EffectiveConfig,
    java: Option<&dyn JavaServiceApi>,
    major: Option<u32>,
) -> (String, String) {
    if let Some(path) = effective
        .java_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return (path.to_string(), "explicit".to_string());
    }
    if let Some(java) = java {
        // 扫描失败不该让整份启动计划失败：那条路本来就只是"挑一个"
        if let Ok(Some(runtime)) = java.pick(major, None) {
            return (runtime.path, "auto".to_string());
        }
    }
    ("java".to_string(), "fallback".to_string())
}

fn substitute(token: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(token.len());
    let mut rest = token;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                if let Some(value) = vars.get(key) {
                    out.push_str(value);
                } else {
                    // 未知变量原样保留，方便排查
                    out.push_str("${");
                    out.push_str(key);
                    out.push('}');
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// MC 的 gameDir 参数字符串：Windows 下用 `\` 更稳，其它平台原样。
fn games_dir_string(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s
    }
}

fn parse_window(window: &str) -> (String, String) {
    let cleaned = window.replace('×', "x").replace('*', "x");
    if cleaned.eq_ignore_ascii_case("fullscreen") {
        return ("0".into(), "0".into());
    }
    let mut parts = cleaned.split('x');
    let w = parts.next().unwrap_or("").trim().to_string();
    let h = parts.next().unwrap_or("").trim().to_string();
    let w = if w.is_empty() { "1280".to_string() } else { w };
    let h = if h.is_empty() { "720".to_string() } else { h };
    (w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".minecraft");
        std::fs::create_dir_all(root.join("versions/1.21.4")).unwrap();
        std::fs::create_dir_all(root.join("libraries/com/mojang/logging/1.21.4")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();

        let vanilla = json!({
            "id": "1.21.4",
            "type": "release",
            "mainClass": "net.minecraft.client.main.Main",
            "assets": "17",
            "assetIndex": { "id": "17" },
            "downloads": { "client": { "url": "https://example/client.jar" } },
            "libraries": [
                { "name": "com.mojang:logging:1.21.4",
                  "downloads": { "artifact": { "path": "com/mojang/logging/1.21.4/logging-1.21.4.jar" } } },
                { "name": "com.mojang:win-only:1",
                  "rules": [{ "action": "allow", "os": { "name": "windows" } }],
                  "downloads": { "artifact": { "path": "com/mojang/win-only/1/win-only-1.jar" } } },
                { "name": "com.mojang:mac-only:1",
                  "rules": [{ "action": "allow", "os": { "name": "osx" } }],
                  "downloads": { "artifact": { "path": "com/mojang/mac-only/1/mac-only-1.jar" } } }
            ],
            "arguments": {
                "jvm": [
                    { "rules": [{ "action": "allow", "os": { "name": "windows" } }],
                      "value": ["-Dos.name=Windows"] },
                    "-cp", "${classpath}"
                ],
                "game": [
                    "--username", "${auth_player_name}",
                    "--uuid", "${auth_uuid}",
                    "--gameDir", "${game_directory}",
                    "--assetIndex", "${assets_index_name}",
                    "--assetsDir", "${assets_root}"
                ]
            }
        });
        std::fs::write(
            root.join("versions/1.21.4/1.21.4.json"),
            serde_json::to_string_pretty(&vanilla).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("versions/1.21.4/1.21.4.jar"), b"jar").unwrap();

        (dir, root)
    }

    fn test_account() -> Account {
        Account {
            id: "acc".into(),
            kind: AccountKind::Offline,
            name: "Player_123".into(),
            uuid: "b50ad385-829d-3141-a216-7e7d7539ba7f".into(),
            skin_model: "default".into(),
            skin_url: None,
            last_used: None,
        }
    }

    fn effective(isolated: bool) -> EffectiveConfig {
        EffectiveConfig {
            memory_mb: 4096,
            java_path: None,
            jvm_args: vec!["-XX:+UseG1GC".into()],
            window: "1280×720".into(),
            check_before_launch: true,
            launch_brand: Some("Kaleid MC Launcher".into()),
            isolated,
            isolated_from: "本文件夹设置".into(),
            jvm_args_mode: kaleid_api::JVM_ARGS_MODE_APPEND.into(),
            gc_mode: kaleid_api::GC_MODE_G1.into(),
            high_performance_gpu: false,
            auto_chinese: false,
            write_latest_launch_bat: true,
        }
    }

    #[test]
    fn maven_name_falls_back_to_a_libraries_path() {
        use crate::install::library_path_from_name;
        // 官方 / 第三方 profile 里最常见的三种写法
        assert_eq!(
            library_path_from_name("net.fabricmc:fabric-loader:0.18.3").as_deref(),
            Some("net/fabricmc/fabric-loader/0.18.3/fabric-loader-0.18.3.jar")
        );
        assert_eq!(
            library_path_from_name("org.ow2.asm:asm:9.9").as_deref(),
            Some("org/ow2/asm/asm/9.9/asm-9.9.jar")
        );
        // 带 classifier（natives）与 `@zip` 后缀
        assert_eq!(
            library_path_from_name("org.lwjgl:lwjgl:3.3.3:natives-windows").as_deref(),
            Some("org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar")
        );
        assert_eq!(
            library_path_from_name("com.example:modpack:1.0@zip").as_deref(),
            Some("com/example/modpack/1.0/modpack-1.0.zip")
        );
        // 推不出来就不硬猜
        assert_eq!(library_path_from_name("nonsense"), None);
        assert_eq!(library_path_from_name("a:b"), None);
        assert_eq!(library_path_from_name(":b:1"), None);
    }

    /// 只有 maven 坐标、没有 `downloads.artifact` 的库必须照样进 classpath，
    /// 且同一个库的两个版本只能留一个。
    ///
    /// 这是真机上「什么版本都起不来」的两个元凶：PCL / HMCL 写出来的加载器
    /// profile 就是只给坐标，漏掉 fabric-loader 后 `KnotClient` 找不到；而原版那份
    /// `asm:9.6` 和加载器那份 `asm:9.9` 同时上 classpath 时 Fabric 会以
    /// `duplicate ASM classes found on classpath` 拒绝启动。
    #[test]
    fn coordinate_only_libraries_are_kept_and_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".minecraft");
        std::fs::create_dir_all(root.join("versions/mixed")).unwrap();
        std::fs::create_dir_all(root.join("libraries/net/fabricmc/fabric-loader/0.18.3")).unwrap();
        std::fs::create_dir_all(root.join("libraries/org/ow2/asm/asm/9.9")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();

        let mixed = json!({
            "id": "mixed",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "assetIndex": { "id": "27" },
            "downloads": { "client": { "url": "https://example/client.jar" } },
            "libraries": [
                // 原版那种带完整 downloads 的写法
                { "name": "org.ow2.asm:asm:9.6",
                  "downloads": { "artifact": { "path": "org/ow2/asm/asm/9.6/asm-9.6.jar" } } },
                // 加载器 profile 那种只给坐标的写法（这是新版本，应当赢）
                { "name": "org.ow2.asm:asm:9.9", "url": "https://maven.fabricmc.net/" },
                { "name": "net.fabricmc:fabric-loader:0.18.3", "url": "https://maven.fabricmc.net/" }
            ],
            "arguments": { "game": ["--username", "${auth_player_name}"] }
        });
        std::fs::write(
            root.join("versions/mixed/mixed.json"),
            serde_json::to_string_pretty(&mixed).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("versions/mixed/mixed.jar"), b"jar").unwrap();

        let plan = build_plan(
            &VersionStore::new(),
            &root,
            "mixed",
            &test_account(),
            &effective(true),
            None,
        )
        .unwrap();

        assert!(
            plan.classpath
                .iter()
                .any(|p| p.replace('\\', "/").ends_with("fabric-loader-0.18.3.jar")),
            "只给坐标的加载器库被丢掉了：{:?}",
            plan.classpath
        );
        let asm: Vec<&String> = plan
            .classpath
            .iter()
            .filter(|p| p.replace('\\', "/").contains("/asm/asm/"))
            .collect();
        assert_eq!(asm.len(), 1, "同一个库的两个版本不能同时上 classpath：{asm:?}");
        assert!(
            asm[0].contains("9.9"),
            "后出现的版本应当覆盖先出现的：{asm:?}"
        );
    }

    /// 空值参数要成对保留：`--xuid ""` 掉一个就变成 `--xuid --versionType`，
    /// 整条命令行错位。`${clientid}` 也必须被替换掉，不能把占位符原样传下去。
    #[test]
    fn empty_argument_values_survive_and_clientid_is_substituted() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".minecraft");
        std::fs::create_dir_all(root.join("versions/v")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::create_dir_all(root.join("libraries")).unwrap();

        let value = json!({
            "id": "v",
            "mainClass": "net.minecraft.client.main.Main",
            "assetIndex": { "id": "27" },
            "downloads": { "client": { "url": "https://example/client.jar" } },
            "arguments": { "game": [
                "--clientId", "${clientid}",
                "--xuid", "${auth_xuid}",
                "--versionType", "${version_type}"
            ] }
        });
        std::fs::write(
            root.join("versions/v/v.json"),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("versions/v/v.jar"), b"jar").unwrap();

        let plan = build_plan(
            &VersionStore::new(),
            &root,
            "v",
            &test_account(),
            &effective(false),
            None,
        )
        .unwrap();

        let args = &plan.game_args;
        let client_id = args.iter().position(|a| a == "--clientId").unwrap();
        assert_eq!(args[client_id + 1], "", "`--clientId` 的值被吞了");
        let xuid = args.iter().position(|a| a == "--xuid").unwrap();
        assert_eq!(args[xuid + 1], "", "`--xuid` 的值被吞了");
        assert!(!args.iter().any(|a| a.starts_with("${")), "还有占位符没替换：{args:?}");
        // 渲染出来的命令行也要能看出这里是个"空参数位"
        assert!(plan.command_line().contains("--xuid \"\""));
    }

    #[test]
    fn plan_substitutes_variables_and_filters_rules() {
        let (_dir, root) = fixture();
        let store = VersionStore::new();
        let plan = build_plan(
            &store,
            &root,
            "1.21.4",
            &test_account(),
            &effective(true),
            None,
        )
        .unwrap();

        assert_eq!(plan.main_class, "net.minecraft.client.main.Main");
        assert_eq!(plan.username, "Player_123");
        assert_eq!(plan.asset_index.as_deref(), Some("17"));
        // uuid 以无连字符形式传给游戏
        assert!(plan
            .game_args
            .contains(&"b50ad385829d3141a2167e7d7539ba7f".to_string()));
        // 隔离开启：gameDir 指向 versions/<id>
        assert!(
            plan.game_dir.ends_with("versions\\1.21.4")
                || plan.game_dir.ends_with("versions/1.21.4")
        );
        assert!(plan
            .classpath
            .iter()
            .any(|p| p.ends_with("logging-1.21.4.jar")));

        if cfg!(windows) {
            assert!(plan.classpath.iter().any(|p| p.ends_with("win-only-1.jar")));
            assert!(!plan.classpath.iter().any(|p| p.ends_with("mac-only-1.jar")));
            assert!(plan.jvm_args.contains(&"-Dos.name=Windows".to_string()));
        }
        // 版本自带的 -cp / ${classpath} 已被摘掉，避免与我们拼的重复
        assert!(!plan.jvm_args.contains(&"-cp".to_string()));

        // 内存与用户 JVM 参数进入 launcher 控制的那一段
        assert!(plan.extra_jvm_args.contains(&"-Xmx4096M".to_string()));
        assert!(plan.extra_jvm_args.contains(&"-XX:+UseG1GC".to_string()));
    }

    /// 那 4 条默认参数是**设置里的内容**，不是隐藏注入：
    /// 设置里有，命令行里就有；设置里删光了，命令行里就该一条都没有。
    #[test]
    fn default_args_come_from_settings_not_from_a_hidden_injection() {
        let (_dir, root) = fixture();
        let store = VersionStore::new();
        let mut cfg = effective(true);
        cfg.gc_mode = kaleid_api::GC_MODE_NONE.into();
        // 1) 设置里就是默认那四条 → 命令行里也正好是它们（顺序照抄）
        cfg.jvm_args = kaleid_api::default_jvm_args();
        cfg.jvm_args.push("-Duser.flag=1".into());
        let plan = build_plan(&store, &root, "1.21.4", &test_account(), &cfg, None).unwrap();

        for arg in kaleid_api::DEFAULT_JVM_ARGS {
            assert!(
                plan.extra_jvm_args.iter().any(|a| a == arg),
                "缺少默认参数 {arg}"
            );
        }
        let default_at = plan
            .extra_jvm_args
            .iter()
            .position(|a| a == "-XX:-OmitStackTraceInFastThrow")
            .unwrap();
        let user_at = plan
            .extra_jvm_args
            .iter()
            .position(|a| a == "-Duser.flag=1")
            .unwrap();
        assert!(default_at < user_at, "设置里的顺序要原样保留");
        // GC 选 none：一条 GC 参数都不该出现
        assert!(plan
            .extra_jvm_args
            .iter()
            .all(|a| !a.starts_with("-XX:+Use")));

        // 2) 用户把那一栏清空 → 命令行里不应再冒出来
        cfg.jvm_args = Vec::new();
        let plan = build_plan(&store, &root, "1.21.4", &test_account(), &cfg, None).unwrap();
        assert!(
            plan.extra_jvm_args
                .iter()
                .all(|a| !kaleid_api::DEFAULT_JVM_ARGS.contains(&a.as_str())),
            "清空之后不该再注入默认参数：{:?}",
            plan.extra_jvm_args
        );
    }

    /// 用户自己写了 GC 开关时，内置档位整体让位，并留下说明。
    #[test]
    fn user_gc_flag_disables_the_builtin_preset() {
        let (_dir, root) = fixture();
        let store = VersionStore::new();
        let mut cfg = effective(true);
        cfg.gc_mode = kaleid_api::GC_MODE_G1_TUNED.into();
        cfg.jvm_args = vec!["-XX:+UseSerialGC".into()];
        let plan = build_plan(&store, &root, "1.21.4", &test_account(), &cfg, None).unwrap();

        assert!(plan
            .extra_jvm_args
            .iter()
            .any(|a| a == "-XX:+UseSerialGC"));
        assert!(!plan.extra_jvm_args.iter().any(|a| a == "-XX:+UseG1GC"));
        assert!(!plan
            .extra_jvm_args
            .iter()
            .any(|a| a == "-XX:G1HeapRegionSize=32M"));
        assert!(plan.jvm_notes.iter().any(|n| n.contains("GC")));
    }

    /// 覆盖模式（override）下全局参数不会出现在生效配置里；
    /// 这里直接验证 `EffectiveConfig` 已经合并好，启动器只管照抄。
    #[test]
    fn gc_preset_shows_up_in_the_command_line() {
        let (_dir, root) = fixture();
        let store = VersionStore::new();
        let mut cfg = effective(true);
        cfg.gc_mode = kaleid_api::GC_MODE_G1_TUNED.into();
        cfg.jvm_args = Vec::new();
        let plan = build_plan(&store, &root, "1.21.4", &test_account(), &cfg, None).unwrap();
        for arg in ["-XX:+UseG1GC", "-XX:G1HeapRegionSize=32M", "-XX:+PerfDisableSharedMem"] {
            assert!(
                plan.extra_jvm_args.iter().any(|a| a == arg),
                "调优 G1GC 缺少 {arg}"
            );
        }
    }

    #[test]
    fn java_version_parsing_handles_old_and_new_schemes() {
        assert_eq!(parse_java_version(r#"java version "1.8.0_402""#), Some(8));
        assert_eq!(
            parse_java_version(r#"openjdk version "17.0.10" 2024-01-16"#),
            Some(17)
        );
        assert_eq!(
            parse_java_version(r#"openjdk version "21" 2023-09-19"#),
            Some(21)
        );
        assert_eq!(
            parse_java_version(r#"openjdk version "24-ea" 2025-03-18"#),
            Some(24)
        );
        assert_eq!(parse_java_version("no version here"), None);
    }

    #[test]
    fn isolation_off_points_game_dir_at_root() {
        let (_dir, root) = fixture();
        let store = VersionStore::new();
        let plan = build_plan(
            &store,
            &root,
            "1.21.4",
            &test_account(),
            &effective(false),
            None,
        )
        .unwrap();
        assert_eq!(Path::new(&plan.game_dir), root);
        assert!(!plan.isolated);
    }

    #[test]
    fn inherited_version_merges_parent_and_uses_parent_jar() {
        let (_dir, root) = fixture();
        std::fs::create_dir_all(root.join("versions/1.21.4-fabric")).unwrap();
        let fabric = json!({
            "id": "1.21.4-fabric",
            "inheritsFrom": "1.21.4",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": [
                { "name": "net.fabricmc:fabric-loader:0.16.9",
                  "downloads": { "artifact": { "path": "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar" } } }
            ]
        });
        std::fs::write(
            root.join("versions/1.21.4-fabric/1.21.4-fabric.json"),
            serde_json::to_string_pretty(&fabric).unwrap(),
        )
        .unwrap();

        let store = VersionStore::new();
        let plan = build_plan(
            &store,
            &root,
            "1.21.4-fabric",
            &test_account(),
            &effective(true),
            None,
        )
        .unwrap();

        assert_eq!(
            plan.main_class,
            "net.fabricmc.loader.impl.launch.knot.KnotClient"
        );
        // 父版本的库与参数仍在
        assert!(plan
            .classpath
            .iter()
            .any(|p| p.ends_with("logging-1.21.4.jar")));
        assert!(plan
            .classpath
            .iter()
            .any(|p| p.ends_with("fabric-loader-0.16.9.jar")));
        // 客户端 jar 取父版本的
        assert!(plan.classpath.iter().any(|p| p.ends_with("1.21.4.jar")));
        assert!(!plan
            .classpath
            .iter()
            .any(|p| p.ends_with("1.21.4-fabric.jar")));
        assert!(plan.game_args.contains(&"--username".to_string()));
    }

    #[test]
    fn missing_missing_main_class_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".minecraft");
        std::fs::create_dir_all(root.join("versions/broken")).unwrap();
        std::fs::write(
            root.join("versions/broken/broken.json"),
            serde_json::to_string(&json!({"id": "broken"})).unwrap(),
        )
        .unwrap();
        let store = VersionStore::new();
        let err = build_plan(
            &store,
            &root,
            "broken",
            &test_account(),
            &effective(true),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Format(_)));
    }

    #[test]
    fn window_variants() {
        assert_eq!(parse_window("854×480"), ("854".into(), "480".into()));
        assert_eq!(parse_window("1280x720"), ("1280".into(), "720".into()));
        assert_eq!(parse_window("fullscreen"), ("0".into(), "0".into()));
        // 缺省时给出安全回退
        assert_eq!(parse_window(""), ("1280".into(), "720".into()));
    }

    #[test]
    fn substitute_leaves_unknown_variables_visible() {
        let mut vars = BTreeMap::new();
        vars.insert("a".to_string(), "1".to_string());
        assert_eq!(substitute("x${a}y", &vars), "x1y");
        assert_eq!(substitute("${nope}", &vars), "${nope}");
    }
}
