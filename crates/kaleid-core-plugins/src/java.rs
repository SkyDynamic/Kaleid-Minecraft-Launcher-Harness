//! Java 运行时发现与挑选（`JavaServiceApi`）。
//!
//! # 为什么要"发现"
//!
//! 用户装 JDK 的位置五花八门：官方启动器自带的、HMCL 装在 `.minecraft/runtime` 的、
//! 注册表登记的、PATH 里的、JetBrains 全家桶附带的 jbr……启动器得把这些都找出来，
//! 才能在"自动匹配"时挑到对的那个，也才能让"手动指定"有列表可选。
//!
//! # 版本号从哪来
//!
//! 读 JDK 根目录的 `release` 文件（`JAVA_VERSION="21.0.5"`），**不 spawn 子进程**：
//! `java -version` 每次上百毫秒，扫十几个候选就会卡住界面。
//! 读不到 `release` 的（多数是 Java 8 的 JRE）标 `valid = false` 仍然列出来，
//! 用户手动选可以用，但自动匹配不碰它。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kaleid_api::{
    ApiError, ApiResult, JavaRuntime, JavaServiceApi, JavaSource, RootRegistryService,
    SettingsServiceApi,
};

use crate::roots::RootRegistry;
use crate::settings::Settings;

/// 缓存多久。JDK 装完不重启启动器也能看到，所以 5 分钟后自动失效。
const CACHE_TTL: Duration = Duration::from_secs(300);
/// 递归找 `bin/javaw.exe` 的最大深度。
const SEARCH_DEPTH: usize = 4;

/// 扫描环境。
///
/// 生产用 [`ScanEnv::current`] 从真实环境变量取，测试直接构造：
/// 读进程环境变量的测试没法并行跑（`set_var` 是全局的）。
#[derive(Debug, Clone, Default)]
pub struct ScanEnv {
    pub java_home: Option<PathBuf>,
    pub path_var: String,
    pub program_files: Vec<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub app_data: Option<PathBuf>,
    /// 各 `.minecraft` 目录（找其中的 `runtime/`）。
    pub game_roots: Vec<PathBuf>,
    /// 设置里手动指定的路径，优先级最高。
    pub explicit: Option<PathBuf>,
}

impl ScanEnv {
    pub fn current(game_roots: Vec<PathBuf>, explicit: Option<PathBuf>) -> Self {
        let env_path = |key: &str| std::env::var_os(key).map(PathBuf::from);
        let mut program_files: Vec<PathBuf> = Vec::new();
        for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(dir) = env_path(key) {
                if !program_files.contains(&dir) {
                    program_files.push(dir);
                }
            }
        }
        ScanEnv {
            java_home: env_path("JAVA_HOME"),
            path_var: std::env::var("PATH").unwrap_or_default(),
            program_files,
            local_app_data: env_path("LOCALAPPDATA"),
            app_data: env_path("APPDATA"),
            game_roots,
            explicit,
        }
    }
}

/// Java 服务。
pub struct JavaService {
    env: ScanEnv,
    settings: Option<Arc<Settings>>,
    roots: Option<Arc<RootRegistry>>,
    cache: Mutex<Option<(Instant, Option<PathBuf>, Vec<JavaRuntime>)>>,
}

impl JavaService {
    pub fn new(env: ScanEnv) -> Self {
        JavaService {
            env,
            settings: None,
            roots: None,
            cache: Mutex::new(None),
        }
    }

    /// 带上设置：`list()` 会把手动指定的路径也列进去（标 `手动指定`）。
    pub fn with_settings(mut self, settings: Arc<Settings>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// 带上文件夹注册表：连带扫各 `.minecraft/runtime`。
    pub fn with_roots(mut self, roots: Arc<RootRegistry>) -> Self {
        self.roots = Some(roots);
        self
    }

    /// 本次扫描要考虑的手动指定路径（全局 + 调用方给的）。
    fn explicit_path(&self) -> Option<PathBuf> {
        if let Some(path) = &self.env.explicit {
            return Some(path.clone());
        }
        let global = self
            .settings
            .as_ref()
            .and_then(|s| s.global().ok())
            .and_then(|g| g.java_path)
            .map(PathBuf::from);
        global
    }

    fn game_roots(&self) -> Vec<PathBuf> {
        let mut out = self.env.game_roots.clone();
        if let Some(roots) = &self.roots {
            if let Ok(list) = roots.list() {
                for root in list {
                    let path = PathBuf::from(&root.path);
                    if !out.contains(&path) {
                        out.push(path);
                    }
                }
            }
        }
        out
    }

    /// 所有候选根目录（还没验证过是不是合法 Java）。
    fn candidates(&self) -> Vec<(PathBuf, JavaSource)> {
        let mut out: Vec<(PathBuf, JavaSource)> = Vec::new();

        if let Some(path) = self.explicit_path() {
            out.push((path, JavaSource::Settings));
        }
        if let Some(path) = &self.env.java_home {
            out.push((path.clone(), JavaSource::JavaHome));
        }

        // Mojang 官方启动器自带的运行时
        if let Some(local) = &self.env.local_app_data {
            for package in subdirs(&local.join("Packages")) {
                let name = package
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !name.starts_with("Microsoft.4297127D64EC6") {
                    continue;
                }
                let runtime = package.join("LocalCache/Local/runtime");
                for home in find_homes(&runtime, SEARCH_DEPTH) {
                    out.push((home, JavaSource::MojangLauncher));
                }
            }
        }

        // 各游戏目录里的 runtime（HMCL / PCL2 会把 JRE 装在这）
        for root in self.game_roots() {
            for home in find_homes(&root.join("runtime"), SEARCH_DEPTH) {
                out.push((home, JavaSource::GameRuntime));
            }
        }

        // 注册表登记的 JDK / JRE
        for home in registry_homes() {
            out.push((home, JavaSource::Registry));
        }

        // 常见安装目录
        for program_files in &self.env.program_files {
            out.extend(common_dir_candidates(program_files));
        }
        if let Some(local) = &self.env.local_app_data {
            out.extend(common_dir_candidates(&local.join("Programs")));
        }
        if let Some(app_data) = &self.env.app_data {
            out.extend(common_dir_candidates(&app_data));
        }

        // PATH 里的 java
        for entry in self.env.path_var.split(';') {
            let dir = PathBuf::from(entry.trim());
            if entry.trim().is_empty() {
                continue;
            }
            if has_java_exe(&dir) {
                // PATH 里放的是 bin/，真正的 home 是上一层
                let home = match dir.file_name().map(|n| n.to_string_lossy().to_lowercase()) {
                    Some(name) if name == "bin" => dir.parent().map(Path::to_path_buf),
                    _ => Some(dir.clone()),
                };
                if let Some(home) = home {
                    out.push((home, JavaSource::PathVar));
                }
            }
        }

        out
    }
}

impl JavaServiceApi for JavaService {
    fn list(&self, force: bool) -> ApiResult<Vec<JavaRuntime>> {
        let explicit = self.explicit_path();
        if !force {
            if let Ok(guard) = self.cache.lock() {
                if let Some((at, cached_explicit, list)) = guard.as_ref() {
                    if at.elapsed() < CACHE_TTL && *cached_explicit == explicit {
                        return Ok(list.clone());
                    }
                }
            }
        }

        let mut out: Vec<JavaRuntime> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (path, source) in self.candidates() {
            let Ok(mut runtime) = inspect_path(&path) else {
                continue;
            };
            runtime.source = source;
            let key = normalize_key(&runtime.home);
            if seen.iter().any(|k| *k == key) {
                continue;
            }
            seen.push(key);
            out.push(runtime);
        }
        // 高版本在前；同版本按名字
        out.sort_by(|a, b| {
            b.major
                .cmp(&a.major)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some((Instant::now(), explicit, out.clone()));
        }
        Ok(out)
    }

    fn inspect(&self, path: &Path) -> ApiResult<JavaRuntime> {
        let mut runtime = inspect_path(path)?;
        runtime.source = JavaSource::Custom;
        Ok(runtime)
    }

    fn pick(&self, major: Option<u32>, explicit: Option<&str>) -> ApiResult<Option<JavaRuntime>> {
        if let Some(raw) = explicit {
            let raw = raw.trim();
            if !raw.is_empty() {
                // 手动指定优先，而且**必须**能用：路径写错要报出来，不能悄悄回退
                return inspect_path(Path::new(raw)).map(Some);
            }
        }
        let list = self.list(false)?;
        Ok(choose(&list, major))
    }
}

/// 从候选里挑一个：精确命中 → 最小的更高版本 → 可用的最高版本。
fn choose(list: &[JavaRuntime], major: Option<u32>) -> Option<JavaRuntime> {
    let usable: Vec<&JavaRuntime> = list.iter().filter(|r| r.valid).collect();
    // 不依赖调用方传入的顺序：`pick` 拿到的可能是缓存里的列表
    let highest = usable
        .iter()
        .max_by_key(|r| r.major)
        .map(|r| (*r).clone())?;
    let Some(major) = major else {
        return Some(highest);
    };
    if let Some(exact) = usable.iter().find(|r| r.major == major) {
        return Some((*exact).clone());
    }
    let mut higher: Vec<&JavaRuntime> =
        usable.iter().copied().filter(|r| r.major > major).collect();
    higher.sort_by_key(|r| r.major);
    if let Some(best) = higher.first() {
        return Some((*best).clone());
    }
    // 本机全是更低的版本：给最高的，总比没有强
    Some(highest)
}

// ---------------------------------------------------------------------------
// 单点校验
// ---------------------------------------------------------------------------

/// 校验一个路径（JDK 根 / `bin` 目录 / `javaw.exe` 都能吃）。
fn inspect_path(path: &Path) -> ApiResult<JavaRuntime> {
    let home = resolve_home(path)?;
    let exe = java_exe(&home)
        .ok_or_else(|| ApiError::invalid(format!("{} 里没有 bin\\javaw.exe", home.display())))?;
    let release = read_release(&home.join("release"));
    let (version, major, vendor, arch) = match release {
        Some(info) => (
            info.version.clone(),
            parse_major(&info.version),
            info.implementor.clone(),
            info.arch.clone(),
        ),
        None => (String::new(), 0, String::new(), String::new()),
    };
    let valid = major > 0;
    // 展示名只要「Java 21」这一层：用户挑运行时靠的是主版本能不能满足版本要求，
    // `Azul Systems, Inc. 21.0.11` 这种把 vendor 与完整版本号塞进一行的写法
    // 在列表里只会挤成一团。vendor / version 仍是字段，界面放到 tooltip 里。
    let name = if major > 0 {
        format!("Java {major}")
    } else {
        match (vendor.is_empty(), version.is_empty()) {
            (false, false) => format!("{vendor} {version}"),
            (false, true) => vendor.clone(),
            (true, false) => format!("Java {version}"),
            (true, true) => home
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Java".to_string()),
        }
    };
    Ok(JavaRuntime {
        path: exe.to_string_lossy().to_string(),
        home: home.to_string_lossy().to_string(),
        name,
        version,
        major,
        vendor,
        arch,
        source: JavaSource::Custom,
        valid,
    })
}

/// 把三种输入统一成 JDK 根目录。
fn resolve_home(path: &Path) -> ApiResult<PathBuf> {
    if path.is_file() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name != "java.exe" && name != "javaw.exe" && name != "java" {
            return Err(ApiError::invalid(format!(
                "不是 Java 可执行文件：{}",
                path.display()
            )));
        }
        // <home>/bin/java.exe → <home>
        return path
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| ApiError::invalid(format!("路径太短：{}", path.display())));
    }
    if !path.is_dir() {
        return Err(ApiError::not_found(format!(
            "路径不存在：{}",
            path.display()
        )));
    }
    if has_java_exe(&path.join("bin")) {
        return Ok(path.to_path_buf());
    }
    // 用户直接选了 bin 目录
    if path.file_name().map(|n| n.to_string_lossy().to_lowercase()) == Some("bin".to_string()) {
        if let Some(parent) = path.parent() {
            if has_java_exe(path) {
                return Ok(parent.to_path_buf());
            }
        }
    }
    Err(ApiError::invalid(format!(
        "{} 里没有 bin\\javaw.exe，不是 Java 目录",
        path.display()
    )))
}

fn has_java_exe(bin: &Path) -> bool {
    bin.join("javaw.exe").is_file() || bin.join("java.exe").is_file()
}

fn java_exe(home: &Path) -> Option<PathBuf> {
    let bin = home.join("bin");
    for name in ["javaw.exe", "java.exe", "java"] {
        let candidate = bin.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Default)]
struct ReleaseInfo {
    version: String,
    implementor: String,
    arch: String,
}

fn read_release(path: &Path) -> Option<ReleaseInfo> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut info = ReleaseInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "JAVA_VERSION" => info.version = value,
            "IMPLEMENTOR" => info.implementor = value,
            "OS_ARCH" => info.arch = value,
            _ => {}
        }
    }
    Some(info)
}

/// `21.0.5` → 21；`1.8.0_392` → 8。
fn parse_major(version: &str) -> u32 {
    let mut parts = version.split(['.', '_', '-', '+']);
    let first = parts.next().unwrap_or("");
    if first == "1" {
        return parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
    }
    first.parse::<u32>().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 目录扫描
// ---------------------------------------------------------------------------

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// 在所有子目录里找"含 `bin/javaw.exe` 的目录"。
fn find_homes(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    if has_java_exe(&root.join("bin")) {
        out.push(root.to_path_buf());
        return out;
    }
    if depth > 0 {
        for child in subdirs(root) {
            out.extend(find_homes(&child, depth - 1));
        }
    }
    out
}

/// 常见安装位置：厂商名子目录 → 逐层找 bin。
fn common_dir_candidates(base: &Path) -> Vec<(PathBuf, JavaSource)> {
    const LAYOUTS: &[&[&str]] = &[
        &["Eclipse Adoptium"],
        &["Java"],
        &["Microsoft"],
        &["Zulu"],
        &["BellSoft"],
        &["Amazon Corretto"],
        &["Semeru"],
        &["Eclipse Foundation"],
        &["JetBrains"],
        &["Android", "Android Studio"],
        &["AdoptOpenJDK"],
        &["RedHat"],
        &["GraalVM"],
    ];
    let mut out = Vec::new();
    for layout in LAYOUTS {
        let mut path = base.to_path_buf();
        for part in *layout {
            path = path.join(part);
        }
        if !path.is_dir() {
            continue;
        }
        // layout 本身可能就是 JDK 根（如 …\Android Studio\jbr）
        if has_java_exe(&path.join("bin")) {
            out.push((path, JavaSource::CommonDir));
            continue;
        }
        for home in find_homes(&path, 2) {
            out.push((home, JavaSource::CommonDir));
        }
    }
    out
}

fn normalize_key(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

// ---------------------------------------------------------------------------
// 注册表
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn registry_homes() -> Vec<PathBuf> {
    registry::homes()
}

#[cfg(not(windows))]
fn registry_homes() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(windows)]
mod registry {
    use std::path::PathBuf;

    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SZ,
    };

    /// 登记 JDK 的厂商键。
    const PATHS: &[&str] = &[
        r"SOFTWARE\JavaSoft\JDK",
        r"SOFTWARE\JavaSoft\Java Development Kit",
        r"SOFTWARE\JavaSoft\Java Runtime Environment",
        r"SOFTWARE\Eclipse Adoptium\JDK",
        r"SOFTWARE\Eclipse Adoptium\JRE",
        r"SOFTWARE\AdoptOpenJDK\JDK",
        r"SOFTWARE\Microsoft\JDK",
        r"SOFTWARE\Azul Systems\Zulu",
        r"SOFTWARE\BellSoft\Liberica",
    ];

    pub fn homes() -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                for path in PATHS {
                    for home in read_family(root, path, view) {
                        out.push(home);
                    }
                }
            }
        }
        out
    }

    /// `…\JDK` 下面是一串版本号子键，每个子键里有 `JavaHome`。
    fn read_family(root: HKEY, path: &str, view: u32) -> Vec<PathBuf> {
        let Some(parent) = open(root, path, view) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for version_key in subkeys(parent) {
            if let Some(child) = open(parent, &version_key, view) {
                if let Some(home) = read_string(child, "JavaHome") {
                    out.push(PathBuf::from(home));
                }
                unsafe { RegCloseKey(child) };
            }
        }
        unsafe { RegCloseKey(parent) };
        out
    }

    fn open(root: HKEY, path: &str, view: u32) -> Option<HKEY> {
        let wide = wide(path);
        let mut handle: HKEY = 0;
        let status = unsafe {
            RegOpenKeyExW(
                root,
                wide.as_ptr(),
                0,
                KEY_READ | view,
                &mut handle as *mut HKEY,
            )
        };
        if status == 0 {
            Some(handle)
        } else {
            None
        }
    }

    fn subkeys(key: HKEY) -> Vec<String> {
        let mut out = Vec::new();
        let mut index = 0u32;
        loop {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let status = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    buf.as_mut_ptr(),
                    &mut len as *mut u32,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
            index += 1;
            if index > 256 {
                break;
            }
        }
        out
    }

    fn read_string(key: HKEY, name: &str) -> Option<String> {
        let wide_name = wide(name);
        let mut kind = 0u32;
        let mut len = 0u32;
        let status = unsafe {
            RegQueryValueExW(
                key,
                wide_name.as_ptr(),
                std::ptr::null(),
                &mut kind as *mut u32,
                std::ptr::null_mut(),
                &mut len as *mut u32,
            )
        };
        if status != 0 || kind != REG_SZ || len == 0 || len > 8192 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let status = unsafe {
            RegQueryValueExW(
                key,
                wide_name.as_ptr(),
                std::ptr::null(),
                &mut kind as *mut u32,
                buf.as_mut_ptr(),
                &mut len as *mut u32,
            )
        };
        if status != 0 {
            return None;
        }
        let units: Vec<u16> = buf
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|u| *u != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个假 JDK：`bin/javaw.exe` + `bin/java.exe` + `release`。
    fn fake_jdk(base: &Path, name: &str, version: &str, implementor: &str) -> PathBuf {
        let home = base.join(name);
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/javaw.exe"), b"exe").unwrap();
        std::fs::write(home.join("bin/java.exe"), b"exe").unwrap();
        std::fs::write(
            home.join("release"),
            format!(
                "IMPLEMENTOR=\"{implementor}\"\nJAVA_VERSION=\"{version}\"\nOS_ARCH=\"x86_64\"\n"
            ),
        )
        .unwrap();
        home
    }

    #[test]
    fn release_file_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let home = fake_jdk(dir.path(), "jdk-21", "21.0.5", "Eclipse Adoptium");
        let runtime = inspect_path(&home).unwrap();
        assert_eq!(runtime.version, "21.0.5");
        assert_eq!(runtime.major, 21);
        assert_eq!(runtime.vendor, "Eclipse Adoptium");
        assert_eq!(runtime.arch, "x86_64");
        assert_eq!(runtime.name, "Java 21");
        assert!(runtime.valid);
        assert!(runtime.path.ends_with("javaw.exe"));
    }

    #[test]
    fn inspect_accepts_home_bin_and_exe() {
        let dir = tempfile::tempdir().unwrap();
        let home = fake_jdk(dir.path(), "jdk-17", "17.0.13", "Microsoft");
        for input in [
            home.clone(),
            home.join("bin"),
            home.join("bin/javaw.exe"),
            home.join("bin/java.exe"),
        ] {
            let runtime = inspect_path(&input).unwrap();
            assert_eq!(runtime.major, 17, "{}", input.display());
            assert_eq!(
                normalize_key(&runtime.home),
                normalize_key(&home.to_string_lossy())
            );
        }
    }

    #[test]
    fn inspect_rejects_non_java_paths() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("nothing");
        std::fs::create_dir_all(&empty).unwrap();
        let err = inspect_path(&empty).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");

        let missing = dir.path().join("no-such-dir");
        let err = inspect_path(&missing).unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");

        let other = dir.path().join("readme.txt");
        std::fs::write(&other, b"x").unwrap();
        let err = inspect_path(&other).unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn java_without_release_is_listed_but_not_valid() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("jre8");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/javaw.exe"), b"exe").unwrap();

        let svc = JavaService::new(ScanEnv {
            explicit: Some(home.clone()),
            ..Default::default()
        });
        let list = svc.list(true).unwrap();
        assert_eq!(list.len(), 1);
        assert!(!list[0].valid);
        assert_eq!(list[0].major, 0);
        // 自动匹配不会挑它
        assert!(svc.pick(Some(17), None).unwrap().is_none());
        // 手动指定仍然能用
        let raw = home.to_string_lossy().to_string();
        assert!(svc.pick(None, Some(&raw)).is_ok());
    }

    #[test]
    fn list_dedupes_and_sorts_by_major() {
        let dir = tempfile::tempdir().unwrap();
        // 同一个 JDK 会从 JAVA_HOME 和常见目录两个来源被找到
        let jdk21 = fake_jdk(
            dir.path(),
            "Eclipse Adoptium/jdk-21.0.5",
            "21.0.5",
            "Eclipse Adoptium",
        );
        fake_jdk(dir.path(), "Microsoft/jdk-17.0.13", "17.0.13", "Microsoft");

        let svc = JavaService::new(ScanEnv {
            java_home: Some(jdk21.clone()),
            program_files: vec![dir.path().to_path_buf()],
            ..Default::default()
        });
        let list = svc.list(true).unwrap();
        assert_eq!(list.len(), 2, "重复来源必须去掉：{list:?}");
        assert_eq!(list[0].major, 21, "高版本在前：{list:?}");
        assert_eq!(
            list.iter()
                .filter(|r| normalize_key(&r.home) == normalize_key(&jdk21.to_string_lossy()))
                .count(),
            1
        );
    }

    #[test]
    fn pick_prefers_exact_then_nearest_higher_then_highest() {
        let mk = |major: u32, valid: bool| JavaRuntime {
            path: format!("C:/jdk{major}/bin/javaw.exe"),
            home: format!("C:/jdk{major}"),
            name: format!("Java {major}"),
            version: format!("{major}.0.1"),
            major,
            vendor: "Test".into(),
            arch: "x86_64".into(),
            source: JavaSource::CommonDir,
            valid,
        };
        let list = vec![mk(21, true), mk(17, true), mk(8, false)];

        assert_eq!(choose(&list, Some(17)).unwrap().major, 17);
        // 21 比 17 高但没有 17 时会挑最近的更高版本
        assert_eq!(
            choose(&[mk(21, true), mk(24, true)], Some(17))
                .unwrap()
                .major,
            21
        );
        // 全都比要求低：给最高的
        assert_eq!(
            choose(&[mk(8, true), mk(11, true)], Some(17))
                .unwrap()
                .major,
            11
        );
        // 不指定：最高
        assert_eq!(choose(&list, None).unwrap().major, 21);
        // 无效的不参与
        assert!(choose(&[mk(8, false)], Some(8)).is_none());
    }

    #[test]
    fn parse_major_handles_legacy_versions() {
        assert_eq!(parse_major("21.0.5"), 21);
        assert_eq!(parse_major("1.8.0_392"), 8);
        assert_eq!(parse_major("17"), 17);
        assert_eq!(parse_major(""), 0);
    }
}
