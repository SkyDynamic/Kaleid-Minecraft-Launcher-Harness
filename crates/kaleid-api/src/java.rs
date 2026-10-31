//! Java 运行时：发现、校验、挑选。
//!
//! 启动器必须能说清"我用的是哪个 Java、为什么是它"。所以这里既给**候选列表**
//! （用户手动指定时选），也给 `pick`（自动匹配时按版本要求的 Java 主版本挑）。
//!
//! 版本信息从 JDK 根目录的 `release` 文件读，**不 spawn 子进程**：
//! `java -version` 每次要 100 ms 上下，扫描一屏候选就会卡住界面。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ApiResult;

/// 一个运行时的来源。界面据此解释"这个 Java 是哪来的"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JavaSource {
    /// 设置里手动指定的路径。
    Settings,
    /// `JAVA_HOME` 环境变量。
    JavaHome,
    /// Mojang 官方启动器自带的运行时（`…\Packages\Microsoft.4297127D64EC6_*\runtime`）。
    MojangLauncher,
    /// `.minecraft/runtime`（HMCL / PCL2 之类装的 JRE）。
    GameRuntime,
    /// 注册表里登记的 JDK/JRE。
    Registry,
    /// 常见安装目录扫到的（Adoptium / Java / Zulu / Corretto / jbr …）。
    CommonDir,
    /// `PATH` 里的 `java.exe`。
    PathVar,
    /// 用户临时校验的一个路径（不入缓存）。
    Custom,
}

impl JavaSource {
    pub fn label_zh(self) -> &'static str {
        match self {
            JavaSource::Settings => "手动指定",
            JavaSource::JavaHome => "JAVA_HOME",
            JavaSource::MojangLauncher => "Mojang 官方启动器",
            JavaSource::GameRuntime => "游戏目录 runtime",
            JavaSource::Registry => "注册表",
            JavaSource::CommonDir => "已安装目录",
            JavaSource::PathVar => "PATH",
            JavaSource::Custom => "自定义路径",
        }
    }
}

/// 一个可用的 Java 运行时。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaRuntime {
    /// 可执行文件：优先 `javaw.exe`（不弹控制台），否则 `java.exe`。
    pub path: String,
    /// JDK/JRE 根目录（`bin` 的上一层）。
    pub home: String,
    /// 展示名，例如 `Eclipse Temurin 21.0.5`。
    pub name: String,
    /// `release` 里的 `JAVA_VERSION`，例如 `21.0.5`。
    pub version: String,
    /// 主版本号，自动匹配按它比。
    pub major: u32,
    pub vendor: String,
    /// `x86_64` / `x86` / …
    pub arch: String,
    pub source: JavaSource,
    /// 这个运行时在本机是否能用（文件存在且能读出 `release`）。
    pub valid: bool,
}

/// Java 服务契约。
pub trait JavaServiceApi: Send + Sync {
    /// 扫描候选列表。`force = true` 穿透缓存。
    fn list(&self, force: bool) -> ApiResult<Vec<JavaRuntime>>;
    /// 校验一个用户给的路径（JDK 根 / `bin` / 可执行文件都接受）。
    fn inspect(&self, path: &Path) -> ApiResult<JavaRuntime>;
    /// 为某个 Java 主版本挑一个：精确命中 → 最小的高于要求 → 可用的最高版本。
    ///
    /// `explicit` 非空时只用它（手动指定优先，且不经缓存）。
    fn pick(&self, major: Option<u32>, explicit: Option<&str>) -> ApiResult<Option<JavaRuntime>>;
}
