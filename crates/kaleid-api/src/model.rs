//! 领域模型：文件夹、版本、账号、生效配置、启动计划。
//!
//! 这些类型是内核、插件与界面之间**唯一的公共语言**。任何一侧都不允许自造平行类型。

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};

// ---------------------------------------------------------------------------
// 文件夹（RootRegistry）
// ---------------------------------------------------------------------------

/// 版本隔离的三态覆盖。
///
/// 解析优先级：**文件夹 → 全局 → 内置默认（true）**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// 跟随全局设置。
    #[default]
    Inherit,
    On,
    Off,
}

impl IsolationMode {
    pub fn label_zh(self) -> &'static str {
        match self {
            IsolationMode::Inherit => "继承全局",
            IsolationMode::On => "开启",
            IsolationMode::Off => "关闭",
        }
    }
}

/// 文件夹被收录时的原始形态，决定后续怎么创建/扫描 `.minecraft`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// 用户直接选了 `.minecraft` 本身（或其内部含 `versions/`）。
    BareMinecraft,
    /// 用户选了 `.minecraft` 的父目录。
    ParentWithMc,
    /// 空目录：先收录，真正安装版本时才创建 `.minecraft`。
    EmptyFolder,
    /// 非空且不含 `.minecraft`，收录前需要用户确认。
    OrdinaryDir,
}

/// 一个被收录的 `.minecraft` 根目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub id: String,
    /// 真正的 `.minecraft` 路径。`EmptyFolder` 收录时该目录可能还不存在。
    pub path: String,
    pub kind: RootKind,
    pub alias: String,
    pub added_at: String,
    #[serde(default)]
    pub isolation: IsolationMode,
    /// 首次启动自动收录的（Roaming）。用户移除后不再自动复活。
    #[serde(default)]
    pub auto: bool,
    #[serde(default)]
    pub last_used: Option<String>,
}

impl Root {
    /// 版本目录。
    pub fn versions_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.path).join("versions")
    }

    pub fn minecraft_exists(&self) -> bool {
        std::path::Path::new(&self.path).is_dir()
    }

    /// 某个版本实例的游戏目录（受隔离策略影响）。
    pub fn game_dir(&self, version_id: &str, isolated: bool) -> std::path::PathBuf {
        let base = std::path::Path::new(&self.path);
        if isolated {
            base.join("versions").join(version_id)
        } else {
            base.to_path_buf()
        }
    }
}

/// 解析某个文件夹的隔离策略是否生效。
pub fn resolve_isolation(folder: IsolationMode, global_default: bool) -> bool {
    match folder {
        IsolationMode::Inherit => global_default,
        IsolationMode::On => true,
        IsolationMode::Off => false,
    }
}

// ---------------------------------------------------------------------------
// 版本
// ---------------------------------------------------------------------------

/// 侧栏按原版 / Fabric / Forge / NeoForge / OptiFine 分组，其余进「未识别」折叠组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoaderKind {
    Vanilla,
    Fabric,
    Forge,
    NeoForge,
    /// 1.8.9 / 1.12.2 这类低版本单独装 OptiFine：没有别的加载器，靠 tweaker 起。
    OptiFine,
    /// Quilt / 结构损坏等：v1 只允许「打开目录」。
    Unrecognized,
}

impl LoaderKind {
    pub fn label_zh(self) -> &'static str {
        match self {
            LoaderKind::Vanilla => "原版",
            LoaderKind::Fabric => "Fabric",
            LoaderKind::Forge => "Forge",
            LoaderKind::NeoForge => "NeoForge",
            LoaderKind::OptiFine => "OptiFine",
            LoaderKind::Unrecognized => "未识别",
        }
    }

    /// 稳定的英文标识：写标记目录（`.fabric` / `.forge` / `.neoforge` / `.optifine`）、
    /// 命令行参数和缓存键都用它，避免和中文展示名绑在一起。
    pub fn slug(self) -> &'static str {
        match self {
            LoaderKind::Vanilla => "vanilla",
            LoaderKind::Fabric => "fabric",
            LoaderKind::Forge => "forge",
            LoaderKind::NeoForge => "neoforge",
            LoaderKind::OptiFine => "optifine",
            LoaderKind::Unrecognized => "unrecognized",
        }
    }
}

/// 版本就绪度，供侧栏状态点显示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    /// 客户端 jar / libraries / assets 有缺失。
    Incomplete {
        missing: usize,
    },
    Unknown,
}

/// 扫描出的版本摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionSummary {
    pub id: String,
    pub loader: LoaderKind,
    /// 加载器版本号（Fabric/Forge/NeoForge 才有）。
    pub loader_version: Option<String>,
    /// `inheritsFrom` 指向的父版本。
    pub inherits_from: Option<String>,
    /// 基础原版版本号。
    ///
    /// 自包含的加载器版本没有 `inheritsFrom`，靠安装时写入的自定义键
    /// `kaleidBaseGameVersion` 还原；老的分离布局则来自链上的根。
    #[serde(default)]
    pub game_version: Option<String>,
    pub readiness: Readiness,
    pub java_major: Option<u32>,
    /// 版本 JSON 的绝对路径。
    pub json_path: String,
    pub last_modified: Option<String>,
}

// ---------------------------------------------------------------------------
// 账号
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    /// 微软正版。
    Msa,
    /// 离线档案。
    Offline,
}

impl AccountKind {
    pub fn label_zh(self) -> &'static str {
        match self {
            AccountKind::Msa => "正版账号",
            AccountKind::Offline => "离线账号",
        }
    }
}

/// 账号（**非敏感**部分；令牌从不进入这个结构，也从不落明文文件）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub kind: AccountKind,
    pub name: String,
    pub uuid: String,
    /// `default` / `slim`
    #[serde(default = "default_skin_model")]
    pub skin_model: String,
    /// 皮肤贴图直链（`textures.minecraft.net/texture/<hash>`）。
    ///
    /// 现在是**预留字段**：正版登录插件还没接 profile 皮肤，所以宿主写不出值，
    /// 界面会回落到内置的史蒂夫/亚历克斯。等皮肤查询接上后只需在这里填值，
    /// `accounts.json` 的旧记录因为有 `default` 也不会反序列化失败。
    #[serde(default)]
    pub skin_url: Option<String>,
    #[serde(default)]
    pub last_used: Option<String>,
}

fn default_skin_model() -> String {
    "default".to_string()
}

/// 离线档案名字规则：1–16 位，字母、数字、下划线或汉字。
///
/// 汉字是**故意放开**的：原版客户端只把名字拿去拼 `OfflinePlayer:<name>` 算
/// uuid，本地正版校验根本不存在，所以中文名在离线启动里能用。但服务器端
/// （尤其是正版校验插件 / 白名单 / 部分聊天组件）大概率不接受，界面必须
/// 挂一条黄字警告，把后果讲在前面。
pub const OFFLINE_NAME_PATTERN: &str = "^[A-Za-z0-9_\\u4e00-\\u9fa5]{1,16}$";

/// 校验离线游戏名。
pub fn validate_offline_name(name: &str) -> ApiResult<()> {
    if name.is_empty() {
        return Err(ApiError::invalid("名字不能为空"));
    }
    let len = name.chars().count();
    if len > 16 {
        return Err(ApiError::invalid("名字最多 16 位"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || ('\u{4e00}'..='\u{9fa5}').contains(&c))
    {
        return Err(ApiError::invalid("只能用字母、数字、下划线或汉字"));
    }
    Ok(())
}

/// 离线档案的 uuid：等价于 Java 的 `UUID.nameUUIDFromBytes("OfflinePlayer:" + name)`。
///
/// 注意不是裸 MD5：Java 会把第 7 字节的高 4 位改成 `3`（版本位）、
/// 把第 9 字节的高 2 位改成 `10`（变体位）。少做这一步，得到的 uuid
/// 和服务器/其它启动器算出来的不一致，皮肤与玩家数据就会错位。
pub fn offline_uuid(name: &str) -> String {
    let mut digest = md5_bytes(format!("OfflinePlayer:{name}").as_bytes());
    digest[6] = (digest[6] & 0x0f) | 0x30;
    digest[8] = (digest[8] & 0x3f) | 0x80;
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn md5_bytes(input: &[u8]) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(input);
    let out = h.finalize();
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&out);
    arr
}

// ---------------------------------------------------------------------------
// 生效配置
// ---------------------------------------------------------------------------

// 下载源（游戏文件）。四态，语义就是字面意思，不再靠「auto 顺便也切镜像」隐含约定。

/// 官方优先：先走官方源；连续两个慢速窗口（或失败）再换镜像。
pub const DOWNLOAD_SOURCE_OFFICIAL_FIRST: &str = "official-first";
/// 镜像优先：先走镜像，失败再回官方源。
pub const DOWNLOAD_SOURCE_MIRROR_FIRST: &str = "mirror-first";
/// 仅官方：只用官方源，镜像连碰都不碰。
pub const DOWNLOAD_SOURCE_OFFICIAL_ONLY: &str = "official-only";
/// 仅镜像：只用镜像，官方源连碰都不碰。
pub const DOWNLOAD_SOURCE_MIRROR_ONLY: &str = "mirror-only";

/// 下载源设置是否合法。
pub fn is_valid_download_source(value: &str) -> bool {
    matches!(
        value,
        DOWNLOAD_SOURCE_OFFICIAL_FIRST
            | DOWNLOAD_SOURCE_MIRROR_FIRST
            | DOWNLOAD_SOURCE_OFFICIAL_ONLY
            | DOWNLOAD_SOURCE_MIRROR_ONLY
    )
}

/// 把旧版下载源取值折算成新取值（老配置文件里是 `auto` / `mojang` / `bmclapi`）。
pub fn normalize_download_source(value: &str) -> String {
    match value.trim() {
        "" | "auto" => DOWNLOAD_SOURCE_OFFICIAL_FIRST.to_string(),
        "mojang" => DOWNLOAD_SOURCE_OFFICIAL_ONLY.to_string(),
        "bmclapi" => DOWNLOAD_SOURCE_MIRROR_ONLY.to_string(),
        other => other.to_string(),
    }
}

/// 全局设置（`settings.json`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalSettings {
    /// 内存分配（MB），默认 4096。
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u32,
    /// 版本隔离的**全局默认**，内置默认 true。
    #[serde(default = "default_true")]
    pub isolation_default: bool,
    #[serde(default = "default_lang")]
    pub language: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 下载并发。
    #[serde(default = "default_concurrency")]
    pub download_concurrency: u32,
    /// 下载源（游戏文件）：`official-first` / `mirror-first` / `official-only` / `mirror-only`。
    #[serde(default = "default_source")]
    pub download_source: String,
    /// 模组数据源：`official-first` / `mirror-first` / `official-only`。
    ///
    /// CurseForge 的官方接口必须带 API Key，所以三种取值下 CurseForge 都走 MCIM 镜像。
    #[serde(default = "default_mod_source")]
    pub mod_source: String,
    /// 模组列表里的注释（简介）显示：`original` 原文 / `translated` MCIM 译文。
    ///
    /// 选译文时**不论 `mod_source` 选了什么**都会请求 MCIM 的翻译接口。
    #[serde(default = "default_mod_description")]
    pub mod_description: String,
    #[serde(default = "default_true")]
    pub check_before_launch: bool,
    /// 最近使用过的 Java 路径，未指定时由 JavaService 自动匹配。
    #[serde(default)]
    pub java_path: Option<String>,
    /// 是否在游戏里显示启动器标识（对应 `--versionType`）。
    ///
    /// 关掉时启动参数里**整对移除** `--versionType`：留着空值的话 MC 会把
    /// `${version_type}` 或空串原样画在版本行上（PCL 的 ModLaunch 也踩过这个坑）。
    #[serde(default = "default_true")]
    pub launch_brand_enabled: bool,
    /// 自定义标识文本，默认 `Kaleid MC Launcher`。
    ///
    /// 空白字符串等价于"隐藏"（解析结果见 [`EffectiveConfig::launch_brand`]）。
    #[serde(default = "default_launch_brand")]
    pub launch_brand: String,
    /// 启动游戏后对启动器自己做什么：`keep` / `minimize` / `hide_to_tray` / `exit`。
    #[serde(default = "default_launch_after_action")]
    pub launch_after_action: String,
    /// 全局 JVM 参数（一行一条，最后由启动器拼到命令行里）。
    ///
    /// 默认就是 [`DEFAULT_JVM_ARGS`] 那四条；输入框里能看到、能改、能删，
    /// 点「恢复默认」会把这四条再填回来。所以"默认参数"不是隐藏注入，
    /// 而是这份设置的第一版内容——用户对命令行有完全的控制权。
    ///
    /// 和版本级的区别：这里的永远会进命令行；版本级可以选择"追加"到它后面，
    /// 或者"覆盖"掉它（见 [`VersionSettings::jvm_args_mode`]）。
    #[serde(default = "default_jvm_args")]
    pub jvm_args: Vec<String>,
    /// 「JVM 参数」这一栏是否已经经历过"填默认值"这一步。
    ///
    /// 只在**老配置文件**上起作用：这些文件里存的是 `jvmArgs: []`（那时默认参数
    /// 由启动器暗中注入，不在设置里），读盘时要把那四条补进设置，否则用户打开
    /// 高级设置会看到一个空框，而命令行里其实一直带着参数。
    ///
    /// 补过一次就把这个标记置上，之后 `[]` 就是"我自己删光了"，不会再填回去。
    #[serde(default)]
    pub jvm_args_initialized: bool,
    /// GC 策略：`zgc` / `zgc_gen` / `g1` / `g1_tuned` / `none`。
    ///
    /// 默认 `g1`（标准 G1GC，Mojang 官方启动器用的那一组）：它是唯一"在任何
    /// JDK 上都能跑、行为又和官方一致"的档位，不会因为用户手动挑了 Java 8 就
    /// 悄悄降级。想更激进可以自己切到 ZGC 或调优 G1GC。
    #[serde(default = "default_gc_mode")]
    pub gc_mode: String,
    /// 是否把启动器与 Java 都设成"使用高性能显卡"（仅 Windows，写注册表）。
    ///
    /// 默认开；非 Windows 上这个值会被 [`effective_high_performance_gpu`] 折成
    /// `false`——那边没有这个注册表项，留着"已开启"只会骗人。
    #[serde(default = "default_true")]
    pub high_performance_gpu: bool,
    /// 启动前把游戏的 `options.txt` 里的语言改成简体中文。默认开。
    #[serde(default = "default_true")]
    pub auto_chinese: bool,
    /// 每次启动都写一份 `<数据目录>/LatestLaunch.bat`（PCL2 的做法，方便对照排查）。
    #[serde(default = "default_true")]
    pub write_latest_launch_bat: bool,
}

/// 默认的游戏内标识文本。
pub const LAUNCH_BRAND_DEFAULT: &str = "Kaleid MC Launcher";
/// 标识文本最长字符数（太长会把版本行撑爆，也会被 MC 截断）。
pub const LAUNCH_BRAND_MAX_CHARS: usize = 48;

/// 校验自定义标识：长度与字符集。
///
/// 允许空串（= 隐藏），但空白内容必须由调用方先 trim；这里只拦真正会破坏
/// 启动参数的字符：换行、制表符、NUL 会被 `Command` 当成参数分隔。
pub fn validate_launch_brand(text: &str) -> ApiResult<()> {
    if text.chars().count() > LAUNCH_BRAND_MAX_CHARS {
        return Err(ApiError::invalid(format!(
            "游戏内标识最多 {LAUNCH_BRAND_MAX_CHARS} 个字符"
        )));
    }
    if text
        .chars()
        .any(|c| c == '\n' || c == '\r' || c == '\t' || c == '\0')
    {
        return Err(ApiError::invalid("游戏内标识不能包含换行、制表符或空字符"));
    }
    Ok(())
}

/// 启动器在游戏启动后自己的动作。
pub const LAUNCH_AFTER_KEEP: &str = "keep";
pub const LAUNCH_AFTER_MINIMIZE: &str = "minimize";
pub const LAUNCH_AFTER_HIDE_TO_TRAY: &str = "hide_to_tray";
pub const LAUNCH_AFTER_EXIT: &str = "exit";

pub fn is_valid_launch_after_action(value: &str) -> bool {
    matches!(
        value,
        LAUNCH_AFTER_KEEP | LAUNCH_AFTER_MINIMIZE | LAUNCH_AFTER_HIDE_TO_TRAY | LAUNCH_AFTER_EXIT
    )
}

/// 非法/缺省一律回到「保持不变」。
pub fn normalize_launch_after_action(value: &str) -> String {
    let value = value.trim();
    if is_valid_launch_after_action(value) {
        value.to_string()
    } else {
        LAUNCH_AFTER_KEEP.to_string()
    }
}

/// GC 策略取值。前两个是 ZGC，中间两个是 G1，最后一个是"什么都不加"。
pub const GC_MODE_ZGC: &str = "zgc";
pub const GC_MODE_ZGC_GENERATIONAL: &str = "zgc_gen";
pub const GC_MODE_G1: &str = "g1";
pub const GC_MODE_G1_TUNED: &str = "g1_tuned";
pub const GC_MODE_NONE: &str = "none";

pub fn is_valid_gc_mode(value: &str) -> bool {
    matches!(
        value,
        GC_MODE_ZGC | GC_MODE_ZGC_GENERATIONAL | GC_MODE_G1 | GC_MODE_G1_TUNED | GC_MODE_NONE
    )
}

/// 非法/缺省一律回到默认档（分代 ZGC）。
pub fn normalize_gc_mode(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    if is_valid_gc_mode(&value) {
        value
    } else {
        GC_MODE_ZGC_GENERATIONAL.to_string()
    }
}

/// 版本级 JVM 参数怎么和全局参数合：`append` 追加 / `override` 覆盖。
pub const JVM_ARGS_MODE_APPEND: &str = "append";
pub const JVM_ARGS_MODE_OVERRIDE: &str = "override";

pub fn is_valid_jvm_args_mode(value: &str) -> bool {
    matches!(value, JVM_ARGS_MODE_APPEND | JVM_ARGS_MODE_OVERRIDE)
}

pub fn normalize_jvm_args_mode(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    if is_valid_jvm_args_mode(&value) {
        value
    } else {
        JVM_ARGS_MODE_APPEND.to_string()
    }
}

fn default_memory_mb() -> u32 {
    4096
}

fn default_true() -> bool {
    true
}
fn default_lang() -> String {
    "zh-CN".into()
}
fn default_theme() -> String {
    "dark".into()
}
fn default_concurrency() -> u32 {
    8
}
fn default_source() -> String {
    DOWNLOAD_SOURCE_OFFICIAL_FIRST.into()
}
fn default_mod_source() -> String {
    crate::mods::MOD_SOURCE_OFFICIAL_FIRST.into()
}
fn default_mod_description() -> String {
    crate::mods::MOD_DESCRIPTION_ORIGINAL.into()
}
fn default_launch_brand() -> String {
    LAUNCH_BRAND_DEFAULT.into()
}
fn default_launch_after_action() -> String {
    LAUNCH_AFTER_KEEP.into()
}
fn default_gc_mode() -> String {
    GC_MODE_G1.into()
}

/// 「使用高性能显卡」这个开关在当前系统上有没有意义。
///
/// 实现是 Windows 的 `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`：
/// Linux / macOS 上没有"用独显跑某个 exe"这回事（那是驱动 / 系统层面的事），
/// 所以那边开关直接禁用，存着的 `true` 也算作 `false`。
pub const HIGH_PERFORMANCE_GPU_SUPPORTED: bool = cfg!(windows);

/// 把设置里存的值折算成"这台机器上真的会生效的值"。
pub fn effective_high_performance_gpu(stored: bool) -> bool {
    stored && HIGH_PERFORMANCE_GPU_SUPPORTED
}
fn default_jvm_args_mode() -> String {
    JVM_ARGS_MODE_APPEND.into()
}

/// 默认的全局 JVM 参数（= 设置里那一栏的初始内容，可改可删）。
///
/// 每一条都有具体理由，不是"看起来专业"：
/// - `-XX:-OmitStackTraceInFastThrow`：JIT 反复抛同一个异常时会省略栈，崩溃日志里
///   只剩一句 `Exception` —— 排查崩溃时最恨的就是这个。
/// - `-Djdk.lang.Process.allowAmbiguousCommands=True`：JDK 18+ 收紧了
///   `ProcessBuilder` 的命令行解析；不少 Forge 老版本（含打包进 mod 的安装器）在
///   带空格的路径下会因此抛 `IOException`。
/// - `-Dfml.ignoreInvalidMinecraftCertificates=True` / `-Dfml.ignorePatchDiscrepancies=True`：
///   Forge 官方文档给的开关，装过补丁 / 换过 Java 之后老版本经常因校验不过而拒绝启动。
pub const DEFAULT_JVM_ARGS: &[&str] = &[
    "-XX:-OmitStackTraceInFastThrow",
    "-Djdk.lang.Process.allowAmbiguousCommands=True",
    "-Dfml.ignoreInvalidMinecraftCertificates=True",
    "-Dfml.ignorePatchDiscrepancies=True",
];

/// [`DEFAULT_JVM_ARGS`] 的 `Vec<String>` 版本，给 serde 默认值与命令返回值用。
pub fn default_jvm_args() -> Vec<String> {
    DEFAULT_JVM_ARGS.iter().map(|arg| (*arg).to_string()).collect()
}

impl Default for GlobalSettings {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).expect("默认值必须可反序列化")
    }
}

/// 某个版本的独立设置（`version-settings/<rootId>/<versionId>.json`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionSettings {
    #[serde(default)]
    pub memory_mb: Option<u32>,
    #[serde(default)]
    pub java_path: Option<String>,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    /// 854×480 / 1280×720 / fullscreen
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub check_before_launch: Option<bool>,
    /// 游戏内标识显隐；`None` = 继承上一层。
    #[serde(default)]
    pub launch_brand_enabled: Option<bool>,
    /// 游戏内标识文本；`None` = 继承上一层。
    #[serde(default)]
    pub launch_brand: Option<String>,
    /// 版本自己的 JVM 参数。
    ///
    /// 怎么和全局参数合，由 [`VersionSettings::jvm_args_mode`] 决定：默认追加，
    /// 选择覆盖时全局参数**整段不进命令行**（GC 与内置默认参数仍然生效，
    /// 它们不是"全局设置"，是启动器的基线）。
    #[serde(default)]
    pub jvm_args_mode: String,
    /// 这个版本单独指定 GC；`None` = 跟随全局。
    #[serde(default)]
    pub gc_mode: Option<String>,
}

/// 三层解析后的生效配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveConfig {
    pub memory_mb: u32,
    pub java_path: Option<String>,
    /// 合并后的 JVM 参数（已按 `jvm_args_mode` 处理过追加/覆盖）。
    pub jvm_args: Vec<String>,
    pub window: String,
    pub check_before_launch: bool,
    /// 解析后的游戏内标识：`None` = 隐藏；`Some(text)` = 显示该文本。
    ///
    /// 三层合并 + 「关掉显隐」+ 「文本为空」都在 [`crate::model::resolve_launch_brand`]
    /// 里收口，启动参数与界面显示读的是同一个结论。
    #[serde(default)]
    pub launch_brand: Option<String>,
    /// 解析后的隔离结论（已含文件夹覆盖）。
    pub isolated: bool,
    /// 隔离结论的来源，便于界面解释"为什么是开启的"。
    pub isolated_from: String,
    /// 版本级参数是怎么合的：`append` / `override`。
    #[serde(default = "default_jvm_args_mode")]
    pub jvm_args_mode: String,
    /// 生效的 GC 策略（版本 > 全局）。
    #[serde(default = "default_gc_mode")]
    pub gc_mode: String,
    #[serde(default)]
    pub high_performance_gpu: bool,
    #[serde(default)]
    pub auto_chinese: bool,
    #[serde(default = "default_true")]
    pub write_latest_launch_bat: bool,
}

/// 把「版本 → 全局」的显隐与文本合成为最终结果。
///
/// - 显隐由最近一层写了显隐的地方决定（都没写 → 显示）；
/// - 文本由最近一层写了非空文本的地方决定（都没写 → 默认标识）；
/// - 显隐为 false，或最终文本 trim 后为空 → `None`（隐藏）。
pub fn resolve_launch_brand(enabled: bool, text: Option<&str>) -> Option<String> {
    if !enabled {
        return None;
    }
    let text = text.map(str::trim).unwrap_or("");
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

// ---------------------------------------------------------------------------
// 启动计划
// ---------------------------------------------------------------------------

/// 一次启动的完整参数（供 CLI 打印快照、测试断言、实际 spawn）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPlan {
    pub version_id: String,
    pub java_path: String,
    /// `java_path` 是怎么来的：`explicit`（设置里指定）/ `auto`（按版本要求的主版本挑的）
    /// / `fallback`（本机没找到可用 Java，退回 PATH 里的 `java`）。
    ///
    /// 界面靠它解释"为什么用这个 Java"，排查"明明装了 JDK 却启动失败"时是第一条线索。
    pub java_source: String,
    pub game_dir: String,
    /// `.minecraft`（或整合包根）目录本身。
    ///
    /// 启动前补全缺失文件时要回读 `versions/<id>/*.json` 与 `assets/indexes/*.json`，
    /// 光有 `libraries_dir` / `assets_dir` 推不出可靠的根，所以单独带一份。
    #[serde(default)]
    pub mc_dir: String,
    pub assets_dir: String,
    pub libraries_dir: String,
    pub isolated: bool,
    pub main_class: String,
    pub classpath: Vec<String>,
    pub jvm_args: Vec<String>,
    pub game_args: Vec<String>,
    pub extra_jvm_args: Vec<String>,
    pub username: String,
    pub uuid: String,
    pub asset_index: Option<String>,
    /// 生效的游戏内标识；`None` = 这次启动不显示标识（参数里已移除 `--versionType`）。
    #[serde(default)]
    pub launcher_brand: Option<String>,
    /// 进程工作目录（= gameDir，可能是隔离目录）。
    #[serde(default)]
    pub launch_dir: String,
    /// natives 解压目录。
    #[serde(default)]
    pub natives_dir: String,
    /// 当前平台需要解压到 [`Self::natives_dir`] 的原生包清单。
    ///
    /// 1.12.2 及更早的版本把原生库放在 `downloads.classifiers` 里，它们既不在
    /// classpath 上，也不在 `downloads.artifact` 里——只靠"扫 classpath 里带
    /// natives 的 jar"根本找不到，`natives/` 于是永远是空的，游戏启动直接
    /// `UnsatisfiedLinkError: no lwjgl in java.library.path`。所以安装、补全、
    /// 校验、解压统一读这份清单（老计划里没有这个字段时按空处理）。
    #[serde(default)]
    pub natives_jars: Vec<NativeJarRef>,
    /// 参与 classpath 的库目录候选：版本本地 `libraries/` 在前，根 `libraries/` 在后。
    #[serde(default)]
    pub libraries_dirs: Vec<String>,
    /// 基础原版版本号（模组热门列表/更新检查的目标版本）。
    #[serde(default)]
    pub game_version: String,
    /// 真实 spawn 时是否注入了访问令牌（计划快照里永远不含令牌本身）。
    #[serde(default)]
    pub access_token_injected: bool,
    /// 生成参数时的说明：GC 因 Java 版本降级、某个参数被覆盖之类的解释。
    ///
    /// 不是错误，但"我设的参数为什么没生效"必须能查到，所以随计划一路带到
    /// [`crate::LaunchState::notes`]，界面直接显示。
    #[serde(default)]
    pub jvm_notes: Vec<String>,
    /// 启动前把游戏语言改成简体中文（写 `options.txt` 的 `lang:` 行）。
    #[serde(default)]
    pub auto_chinese: bool,
    /// 每次启动都写一份 `<数据目录>/LatestLaunch.bat`。
    #[serde(default)]
    pub write_latest_launch_bat: bool,
    /// 把启动器与 Java 设成"使用高性能显卡"（仅 Windows）。
    #[serde(default)]
    pub high_performance_gpu: bool,
}

/// 一个要解压到 `natives/` 的原生包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeJarRef {
    /// `libraries/` 下的相对路径（Maven 布局，正斜杠）。
    pub path: String,
    /// 官方下载地址；离线计划里可能为空（补全时会按 Maven 布局自己拼）。
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    /// 字节数；分类器条目没写大小时是 0（进度条按"文件个数"照样能走）。
    #[serde(default)]
    pub size: u64,
    /// `extract.exclude`：解压时按前缀跳过的条目，典型是 `META-INF/`。
    #[serde(default)]
    pub excludes: Vec<String>,
}

/// 一次完整校验的明细（`versions_verify`）。
///
/// 比 [`Readiness::Incomplete`] 那个"缺 N 个"更具体：到底缺的是 jar、依赖库、
/// 资源索引，还是索引里登记的资源文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrityReport {
    pub version_id: String,
    pub ok: bool,
    pub missing_jar: bool,
    pub missing_libraries: usize,
    pub missing_assets_index: bool,
    pub missing_assets_objects: usize,
    /// 资源索引里登记的对象总数；0 表示没有索引（老版本/未安装）。
    pub assets_object_total: usize,
    pub missing_total: usize,
}

impl LaunchPlan {
    /// 供人读的命令行（日志与测试快照用）。
    pub fn command_line(&self) -> String {
        let mut parts = vec![self.java_path.clone()];
        parts.extend(self.jvm_args.iter().cloned());
        parts.extend(self.extra_jvm_args.iter().cloned());
        parts.push("-cp".into());
        parts.push(self.classpath.join(";"));
        parts.push(self.main_class.clone());
        parts.extend(self.game_args.iter().cloned());
        parts
            .into_iter()
            .map(|p| {
                // 空参数必须渲染成 `""`：`--clientId ""` 这类"值为空但是个参数位"
                // 的情况如果直接印成空串，复制出来就是 `--clientId --xuid`——
                // 参数错位，游戏起不来。
                if p.contains(' ') || p.is_empty() {
                    format!("\"{p}\"")
                } else {
                    p
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// ---------------------------------------------------------------------------
// 文件夹拾取
// ---------------------------------------------------------------------------

/// 用户选定一个路径后，内核判定它属于哪种收录情形。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Pick {
    BareMinecraft { root: String },
    ParentWithMc { parent: String, root: String },
    EmptyFolder { path: String },
    OrdinaryDir { path: String },
}

impl Pick {
    /// 判定一个路径属于哪种情形。
    ///
    /// - 末段是 `.minecraft`，或目录里已有 `versions/` → `BareMinecraft`
    /// - 目录里含 `.minecraft/` → `ParentWithMc`
    /// - 目录为空（或不存在） → `EmptyFolder`
    /// - 其余 → `OrdinaryDir`（需用户确认）
    pub fn classify(path: &std::path::Path) -> Pick {
        let as_str = path.to_string_lossy().to_string();
        let is_mc_named = path
            .file_name()
            .map(|n| n.eq_ignore_ascii_case(".minecraft"))
            .unwrap_or(false);

        if is_mc_named || path.join("versions").is_dir() {
            return Pick::BareMinecraft { root: as_str };
        }
        let nested = path.join(".minecraft");
        if nested.is_dir() {
            return Pick::ParentWithMc {
                parent: as_str,
                root: nested.to_string_lossy().to_string(),
            };
        }
        if !path.exists() {
            return Pick::EmptyFolder { path: as_str };
        }
        match std::fs::read_dir(path) {
            Ok(mut it) => {
                if it.next().is_none() {
                    Pick::EmptyFolder { path: as_str }
                } else {
                    Pick::OrdinaryDir { path: as_str }
                }
            }
            Err(_) => Pick::OrdinaryDir { path: as_str },
        }
    }

    /// 这个路径解析出的 `.minecraft` 根。
    ///
    /// `OrdinaryDir` 也给出一个根：`<path>/.minecraft`（用户在确认后才会用到它）。
    pub fn root_path(&self) -> Option<String> {
        match self {
            Pick::BareMinecraft { root } => Some(root.clone()),
            Pick::ParentWithMc { root, .. } => Some(root.clone()),
            Pick::EmptyFolder { path } => Some(path.clone()),
            Pick::OrdinaryDir { path } => Some(
                std::path::Path::new(path)
                    .join(".minecraft")
                    .to_string_lossy()
                    .to_string(),
            ),
        }
    }

    pub fn kind(&self) -> RootKind {
        match self {
            Pick::BareMinecraft { .. } => RootKind::BareMinecraft,
            Pick::ParentWithMc { .. } => RootKind::ParentWithMc,
            Pick::EmptyFolder { .. } => RootKind::EmptyFolder,
            Pick::OrdinaryDir { .. } => RootKind::OrdinaryDir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_priority_is_folder_then_global_then_builtin() {
        // 文件夹显式设置优先于全局
        assert!(resolve_isolation(IsolationMode::On, false));
        assert!(!resolve_isolation(IsolationMode::Off, true));
        // 继承时取全局
        assert!(resolve_isolation(IsolationMode::Inherit, true));
        assert!(!resolve_isolation(IsolationMode::Inherit, false));
    }

    #[test]
    fn offline_name_rules() {
        assert!(validate_offline_name("Player_123").is_ok());
        assert!(validate_offline_name("a".repeat(16).as_str()).is_ok());
        assert!(validate_offline_name("").is_err());
        assert!(validate_offline_name(&"a".repeat(17)).is_err());
        // 汉字放开：离线启动本身不校验，但服务器侧可能不认（界面会挂警告）。
        assert!(validate_offline_name("玩家名").is_ok());
        assert!(validate_offline_name("中文名ABC_1").is_ok());
        assert!(validate_offline_name(&"玩".repeat(16)).is_ok());
        assert!(validate_offline_name(&"玩".repeat(17)).is_err());
        assert!(validate_offline_name("bad name").is_err());
        assert!(validate_offline_name("bad-name").is_err());
        assert!(validate_offline_name("名字 空格").is_err());
        assert!(validate_offline_name("ひらがな").is_err(), "只放开汉字");
        assert!(validate_offline_name("名字!").is_err());
    }

    #[test]
    fn offline_uuid_matches_java_offline_mode() {
        // 与 Java 的 UUID.nameUUIDFromBytes("OfflinePlayer:Notch") 一致
        assert_eq!(
            offline_uuid("Notch"),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
        // 名字变了 uuid 必须跟着变
        assert_ne!(offline_uuid("Notch"), offline_uuid("Notch2"));
    }

    #[test]
    fn game_dir_follows_isolation() {
        let root = Root {
            id: "r1".into(),
            path: r"C:\mc".into(),
            kind: RootKind::BareMinecraft,
            alias: "默认".into(),
            added_at: "2026-01-01T00:00:00Z".into(),
            isolation: IsolationMode::Inherit,
            auto: true,
            last_used: None,
        };
        assert_eq!(
            root.game_dir("1.21.4", true),
            std::path::Path::new(r"C:\mc\versions\1.21.4")
        );
        assert_eq!(
            root.game_dir("1.21.4", false),
            std::path::Path::new(r"C:\mc")
        );
    }

    #[test]
    fn global_settings_defaults() {
        let s = GlobalSettings::default();
        assert_eq!(s.memory_mb, 4096);
        assert!(s.isolation_default, "版本隔离内置默认必须是开启");
        assert_eq!(s.download_concurrency, 8);
        assert_eq!(s.download_source, DOWNLOAD_SOURCE_OFFICIAL_FIRST);
        assert_eq!(s.mod_source, crate::mods::MOD_SOURCE_OFFICIAL_FIRST);
        assert_eq!(s.mod_description, crate::mods::MOD_DESCRIPTION_ORIGINAL);
        // 用户拍板的三条默认值：标准 G1GC / 高性能显卡开 / 自动中文开
        assert_eq!(s.gc_mode, GC_MODE_G1);
        assert!(s.high_performance_gpu);
        assert!(s.auto_chinese);
        assert!(s.write_latest_launch_bat);
        assert_eq!(s.jvm_args, default_jvm_args());
    }

    #[test]
    fn high_performance_gpu_only_counts_on_windows() {
        // Windows 上原样生效；其它系统存了 true 也按关处理
        assert_eq!(
            effective_high_performance_gpu(true),
            cfg!(windows),
            "非 Windows 上不该出现「已开启高性能显卡」"
        );
        assert!(!effective_high_performance_gpu(false));
        assert_eq!(HIGH_PERFORMANCE_GPU_SUPPORTED, cfg!(windows));
    }

    #[test]
    fn legacy_download_source_values_are_normalized() {
        assert_eq!(
            normalize_download_source("auto"),
            DOWNLOAD_SOURCE_OFFICIAL_FIRST
        );
        assert_eq!(
            normalize_download_source("mojang"),
            DOWNLOAD_SOURCE_OFFICIAL_ONLY
        );
        assert_eq!(
            normalize_download_source("bmclapi"),
            DOWNLOAD_SOURCE_MIRROR_ONLY
        );
        assert_eq!(
            normalize_download_source(""),
            DOWNLOAD_SOURCE_OFFICIAL_FIRST
        );
        // 新取值原样通过
        assert_eq!(
            normalize_download_source(DOWNLOAD_SOURCE_MIRROR_FIRST),
            DOWNLOAD_SOURCE_MIRROR_FIRST
        );
        assert!(is_valid_download_source(DOWNLOAD_SOURCE_MIRROR_FIRST));
        assert!(!is_valid_download_source("auto"));
    }

    #[test]
    fn pick_classification() {
        let tmp = tempfile_dir();
        // 空目录
        assert!(matches!(
            Pick::classify(&tmp.join("empty")),
            Pick::EmptyFolder { .. }
        ));

        // 含 .minecraft 的父目录
        let parent = tmp.join("parent");
        std::fs::create_dir_all(parent.join(".minecraft")).unwrap();
        match Pick::classify(&parent) {
            Pick::ParentWithMc { root, .. } => {
                assert!(root.ends_with(".minecraft"));
            }
            other => panic!("期望 ParentWithMc，实际 {other:?}"),
        }

        // .minecraft 本身
        match Pick::classify(&parent.join(".minecraft")) {
            Pick::BareMinecraft { .. } => {}
            other => panic!("期望 BareMinecraft，实际 {other:?}"),
        }

        // 非空普通目录
        let ordinary = tmp.join("ordinary");
        std::fs::create_dir_all(&ordinary).unwrap();
        std::fs::write(ordinary.join("readme.txt"), b"hi").unwrap();
        assert!(matches!(
            Pick::classify(&ordinary),
            Pick::OrdinaryDir { .. }
        ));
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("kaleid-api-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
