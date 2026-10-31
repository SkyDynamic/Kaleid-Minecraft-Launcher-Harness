//! 本地游戏内容：模组、资源包、存档、日志。
//!
//! 这里的类型描述的是**磁盘上真实存在的东西**，不是远程目录里的条目：
//! 模组就是 `mods/` 下的 jar，存档就是 `saves/<folder>/level.dat`。
//!
//! 位置规则只有一条：**全部跟随版本隔离**。隔离开 → `<根>/versions/<id>/…`，
//! 关 → `<根>/…`。这和启动计划里的 `--gameDir` 是同一个目录，
//! 否则"界面里看到的模组"和"游戏真正加载的模组"就会分叉。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ApiResult;

/// 一次内容读取解析出的目录集合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDirs {
    /// 生效的游戏目录（隔离时是 `versions/<id>`）。
    pub game_dir: String,
    pub mods_dir: String,
    pub resource_packs_dir: String,
    pub saves_dir: String,
    pub logs_dir: String,
    pub isolated: bool,
    /// 隔离结论的来源，便于界面解释"为什么是这个目录"。
    pub isolated_from: String,
}

/// `mods/` 里的一个模组文件。
///
/// 名称/版本来自 jar 内真实元数据（`fabric.mod.json` / `neoforge.mods.toml` / …）；
/// 读不出来时才回落到文件名启发式（`meta_source == "filename"`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalMod {
    /// 真实文件名（含 `.jar.disabled`）。
    pub file_name: String,
    /// 展示名：jar 内真实名 > 名称表中文名 > 文件名。
    pub display_name: String,
    pub version: Option<String>,
    pub size_bytes: u64,
    /// `false` 表示磁盘上是 `.jar.disabled`（改名的停用方式）。
    pub enabled: bool,
    /// 修改时间（epoch 秒）。
    pub modified_at: Option<u64>,
    /// jar 内声明的 mod id。
    #[serde(default)]
    pub mod_id: Option<String>,
    /// jar 内的原始名称（未做中文替换）。
    #[serde(default)]
    pub name: Option<String>,
    /// 元数据里的作者列表。
    #[serde(default)]
    pub authors: Vec<String>,
    /// 声明支持的加载器：`fabric` / `quilt` / `forge` / `neoforge`。
    #[serde(default)]
    pub flavors: Vec<String>,
    /// jar 内是否有可展示的图标（真正的图片由 `content_mod_icon` 按需取）。
    #[serde(default)]
    pub has_icon: bool,
    /// 元数据来源：`fabric.mod.json` / `neoforge.mods.toml` / `manifest` / `filename`。
    #[serde(default)]
    pub meta_source: Option<String>,
}

/// 模组图标（base64 data URL）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModIcon {
    pub data_url: String,
    pub mime: String,
}

/// 某个已装模组的可用更新。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModUpdate {
    /// 当前安装的文件名。
    pub file_name: String,
    pub mod_id: Option<String>,
    /// `modrinth` / `curseforge`
    pub source: String,
    pub project_id: String,
    pub current_version: Option<String>,
    pub latest_version: String,
    pub latest_file_name: String,
    pub download_url: String,
    pub size: u64,
    #[serde(default)]
    pub sha1: Option<String>,
    /// 项目主页，便于用户自己去看更新日志。
    #[serde(default)]
    pub page_url: Option<String>,
}

/// 一次更新检查的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModUpdateReport {
    pub updates: Vec<ModUpdate>,
    /// 查不到来源的模组文件名（不当作错误）。
    pub unknown: Vec<String>,
    pub warnings: Vec<String>,
    pub checked_at: u64,
}

/// `resourcepacks/` 里的一个资源包。
///
/// 启用态是**只读**的：来自 `options.txt` 的 `resourcePacks` 列表，
/// 本启动器不写回这个文件（写了就等于替用户改游戏配置）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPack {
    pub file_name: String,
    pub display_name: String,
    pub size_bytes: u64,
    pub enabled: bool,
    /// `options.txt` 里登记了、但磁盘上已经没有这一项。
    pub missing: bool,
    /// 目录形态的资源包（解开的包）。
    pub is_dir: bool,
    pub modified_at: Option<u64>,
}

/// `saves/` 里的一个存档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSave {
    /// 存档目录名（`level.dat` 所在的那一层）。
    pub folder: String,
    /// `level.dat` 里的 `LevelName`；读不出来时回落到目录名。
    pub name: String,
    /// `survival` / `creative` / `adventure` / `spectator` / `hardcore` / `unknown`
    pub game_mode: String,
    /// `level.dat` 里的 `Version.Name`（存档最后一次被哪个版本写过）。
    pub mc_version: Option<String>,
    /// `LastPlayed`：epoch **秒**（NBT 里存的是毫秒，读取时已换算成秒，
    /// 与 `LogFile.modified_at` 之类保持同一套单位）。
    pub last_played: Option<u64>,
    pub size_bytes: u64,
    /// 命中目录遍历上限，`size_bytes` 只是下界。
    pub size_capped: bool,
    pub has_icon: bool,
}

/// `logs/` 里的一个日志文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFile {
    pub file_name: String,
    pub size_bytes: u64,
    pub modified_at: Option<u64>,
    /// `latest.log`：游戏最近一次运行的日志。
    pub is_latest: bool,
    /// `.log.gz`：旧日志会被游戏压缩。
    pub compressed: bool,
}

/// 一次读取到的日志尾部。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogTail {
    pub file_name: String,
    pub total_bytes: u64,
    pub shown_bytes: u64,
    /// 只给了尾部：界面要明确告诉用户"前面还有"。
    pub truncated: bool,
    pub text: String,
}

/// 导入 / 删除针对的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Mod,
    ResourcePack,
}

impl ContentKind {
    pub fn label_zh(self) -> &'static str {
        match self {
            ContentKind::Mod => "模组",
            ContentKind::ResourcePack => "资源包",
        }
    }

    /// 允许导入的扩展名（小写、不带点）。
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            ContentKind::Mod => &["jar"],
            ContentKind::ResourcePack => &["zip"],
        }
    }
}

/// 被跳过的导入项及原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSkipped {
    pub file: String,
    pub reason: String,
}

/// 一次导入的结果。**不覆盖同名文件**：冲突进 `skipped`，由界面告知用户。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub copied: Vec<String>,
    pub skipped: Vec<ImportSkipped>,
}

/// 本地内容服务：列举、启停、导入、删除。
pub trait ContentServiceApi: Send + Sync {
    fn dirs(&self, root_id: &str, version_id: &str) -> ApiResult<ContentDirs>;
    fn mods(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LocalMod>>;
    /// 改名启停（`.jar` ↔ `.jar.disabled`），返回改动后的完整列表。
    fn set_mod_enabled(
        &self,
        root_id: &str,
        version_id: &str,
        file: &str,
        enabled: bool,
    ) -> ApiResult<Vec<LocalMod>>;
    fn resource_packs(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LocalPack>>;
    fn saves(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LocalSave>>;
    fn logs(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LogFile>>;
    /// 读日志尾部：`max_bytes` 是调用方给的上限，实现方还可以再收紧。
    fn read_log(
        &self,
        root_id: &str,
        version_id: &str,
        file: &str,
        max_bytes: usize,
    ) -> ApiResult<LogTail>;
    fn import(
        &self,
        root_id: &str,
        version_id: &str,
        kind: ContentKind,
        paths: &[PathBuf],
    ) -> ApiResult<ImportOutcome>;
    /// 删除单个文件或整个资源包目录。失败原样上抛（可能被游戏占用）。
    fn delete(
        &self,
        root_id: &str,
        version_id: &str,
        kind: ContentKind,
        file: &str,
    ) -> ApiResult<()>;
}
