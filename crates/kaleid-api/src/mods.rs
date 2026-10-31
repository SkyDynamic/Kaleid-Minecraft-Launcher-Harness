//! 模组下载（Modrinth / CurseForge / MCIM 镜像）的公共数据结构与纯函数。
//!
//! 两侧共享的规则都放在这里，避免界面与插件各写一遍又慢慢跑偏：
//! 版本区间文案、下载量文案、镜像地址映射、加载器标签顺序。

use serde::{Deserialize, Serialize};

use crate::model::LoaderKind;

// ---------------------------------------------------------------------------
// 数据源
// ---------------------------------------------------------------------------

/// Mod 数据源 = 官方优先（Modrinth 先直连，失败回退镜像）。
pub const MOD_SOURCE_OFFICIAL_FIRST: &str = "official-first";
/// Mod 数据源 = 镜像优先（先用 MCIM 镜像，失败回退官方）。
pub const MOD_SOURCE_MIRROR_FIRST: &str = "mirror-first";
/// Mod 数据源 = 仅官方（CurseForge 官方接口需要 API Key，仍走镜像）。
pub const MOD_SOURCE_OFFICIAL_ONLY: &str = "official-only";

/// Mod 数据源设置是否合法。
pub fn is_valid_mod_source(value: &str) -> bool {
    matches!(
        value,
        MOD_SOURCE_OFFICIAL_FIRST | MOD_SOURCE_MIRROR_FIRST | MOD_SOURCE_OFFICIAL_ONLY
    )
}

/// 把旧版取值（`auto` / `mcim` / `official`）折算成新取值。
///
/// 老配置文件里存的是三态；用户升级后不该看到「设置项不合法」，所以读盘时统一
/// 归一化一次：自动 → 官方优先、只用镜像 → 镜像优先、只用官方 → 仅官方。
pub fn normalize_mod_source(value: &str) -> String {
    match value.trim() {
        "" | "auto" => MOD_SOURCE_OFFICIAL_FIRST.to_string(),
        "mcim" => MOD_SOURCE_MIRROR_FIRST.to_string(),
        "official" => MOD_SOURCE_OFFICIAL_ONLY.to_string(),
        other => other.to_string(),
    }
}

/// Mod 列表注释 = 显示站上的原文简介。
pub const MOD_DESCRIPTION_ORIGINAL: &str = "original";
/// Mod 列表注释 = 显示 MCIM 译文（不论数据源选的是什么都会去请求翻译）。
pub const MOD_DESCRIPTION_TRANSLATED: &str = "translated";

/// 列表注释设置是否合法。
pub fn is_valid_mod_description(value: &str) -> bool {
    matches!(value, MOD_DESCRIPTION_ORIGINAL | MOD_DESCRIPTION_TRANSLATED)
}

/// 归一化列表注释设置，非法/缺省一律回到「原文」。
pub fn normalize_mod_description(value: &str) -> String {
    let value = value.trim();
    if is_valid_mod_description(value) {
        value.to_string()
    } else {
        MOD_DESCRIPTION_ORIGINAL.to_string()
    }
}

/// 单个模组来自哪个站点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModSource {
    #[default]
    Modrinth,
    CurseForge,
}

impl ModSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ModSource::Modrinth => "modrinth",
            ModSource::CurseForge => "curseforge",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModSource::Modrinth => "Modrinth",
            ModSource::CurseForge => "CurseForge",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "modrinth" => Some(ModSource::Modrinth),
            "curseforge" => Some(ModSource::CurseForge),
            _ => None,
        }
    }
}

/// 搜索时用户选的来源过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModSourceFilter {
    #[default]
    All,
    Modrinth,
    CurseForge,
}

impl ModSourceFilter {
    pub fn includes(self, source: ModSource) -> bool {
        match self {
            ModSourceFilter::All => true,
            ModSourceFilter::Modrinth => source == ModSource::Modrinth,
            ModSourceFilter::CurseForge => source == ModSource::CurseForge,
        }
    }
}

// ---------------------------------------------------------------------------
// 列表项
// ---------------------------------------------------------------------------

/// 搜索 / 排行列表里的一行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModHit {
    pub source: ModSource,
    /// Modrinth 用 `project_id`，CurseForge 用数字 id 的字符串形式。
    pub id: String,
    pub slug: String,
    /// 站上的原名（英文）。
    pub name: String,
    /// 中文名（来自本地中文名词表；没有就是 `None`）。
    pub translated_name: Option<String>,
    pub description: String,
    /// 简介译文（MCIM 翻译接口；只在详情页请求）。
    pub translated_description: Option<String>,
    pub icon_url: Option<String>,
    pub downloads: u64,
    /// ISO 时间字符串，界面自己格式化。
    pub updated_at: String,
    /// `fabric` / `neoforge` / `forge` / `quilt` 之类的规范小写名。
    pub loaders: Vec<String>,
    /// `Fabric / NeoForge` 这类可直接显示的标签。
    pub loaders_label: String,
    /// `1.20+`、`1.16-1.21` 这类版本区间文案。
    pub game_range: String,
    /// 主类型的中文标签（`性能优化` 之类）。
    pub type_label: String,
    /// 命中分类表的全部中文标签，详情页当 bridge 用。
    pub tags: Vec<String>,
    /// 站内页面。
    pub page_url: String,
    /// MC 百科页面（词表里有 id 才给）。
    pub wiki_url: Option<String>,
}

impl ModHit {
    /// 显示用主名：有中文名时用中文名。
    pub fn display_name(&self) -> &str {
        self.translated_name.as_deref().unwrap_or(&self.name)
    }
}

/// 状态为「已安装」的模组文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledMod {
    pub file_name: String,
    pub size: u64,
    pub sha1: Option<String>,
    /// `file_name`（文件名完全相等）或 `slug`（文件名里出现 slug）。
    pub matched_by: String,
}

// ---------------------------------------------------------------------------
// 搜索
// ---------------------------------------------------------------------------

/// 搜索 / 排行的入参。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSearchQuery {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub source: ModSourceFilter,
    /// 类型表里的 id（`worldgen` 之类）；`None` = 全部类型。
    #[serde(default)]
    pub type_id: Option<String>,
    #[serde(default)]
    pub page_size: Option<u32>,
    /// 0 起的页码；`None` = 第 0 页。与排行榜共用同一页大小（50）。
    ///
    /// 两个数据源的分页参数不同名（Modrinth `offset` / CurseForge `index`），
    /// 但语义一致，所以对外只暴露"第几页"。
    #[serde(default)]
    pub page: Option<u32>,
    /// 强制刷新（忽略缓存，但仍会写缓存）。
    #[serde(default)]
    pub force: bool,
    /// 当前最新正式版；只用于把版本区间收敛成 `1.20+`。
    #[serde(default)]
    pub latest_game_version: Option<String>,
}

/// 搜索结果。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModSearchResult {
    pub hits: Vec<ModHit>,
    pub modrinth_total: i64,
    pub curseforge_total: i64,
    /// 单源失败、镜像未缓存、CurseForge 走镜像之类的说明，界面直接显示。
    pub warnings: Vec<String>,
    /// 结果生成时间（毫秒时间戳），界面用来显示「刚刚」。
    pub fetched_at: u64,
}

// ---------------------------------------------------------------------------
// 详情
// ---------------------------------------------------------------------------

/// 详情页顶部卡片。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModProject {
    pub source: ModSource,
    pub id: String,
    pub slug: String,
    pub name: String,
    pub translated_name: Option<String>,
    pub description: String,
    pub translated_description: Option<String>,
    pub icon_url: Option<String>,
    pub downloads: u64,
    pub updated_at: String,
    pub loaders_label: String,
    pub game_range: String,
    pub tags: Vec<String>,
    pub page_url: String,
    pub wiki_url: Option<String>,
    /// `mods/` 里已经装了同一个模组时给出。
    pub installed: Option<InstalledMod>,
}

/// 详情页里的一个候选文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModFile {
    pub source: ModSource,
    pub project_id: String,
    pub file_id: String,
    /// 站上的版本名（`Iris 1.11.7 for NeoForge 26.3` 之类）。
    pub name: String,
    pub file_name: String,
    pub version_number: String,
    pub size: u64,
    pub downloads: u64,
    pub published_at: String,
    /// `release` / `beta` / `alpha`。
    pub release_type: String,
    pub loaders: Vec<String>,
    pub game_versions: Vec<String>,
    pub official_url: String,
    pub mirror_url: Option<String>,
    /// 站的 SHA-1（有的文件不提供）。
    pub sha1: Option<String>,
    /// 该文件声明的必选前置数量。
    pub required_deps: usize,
}

/// 「加载器 + MC 版本」一组的展开内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModVersionGroup {
    /// 规范小写加载器名。
    pub loader: String,
    pub loader_label: String,
    /// MC 版本家族（`1.20` / `26.3`）。
    pub game_version: String,
    /// 必选前置（已解析成列表项，样式与搜索结果一致）。
    pub dependencies: Vec<ModHit>,
    /// 可选前置。
    pub optional_dependencies: Vec<ModHit>,
    pub files: Vec<ModFile>,
}

/// 详情页的全部数据（一次往返）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModDetail {
    pub project: ModProject,
    pub groups: Vec<ModVersionGroup>,
    /// 版本筛选条用的家族列表（倒序，不含「全部」）。
    pub versions: Vec<String>,
    pub warnings: Vec<String>,
}

/// 详情页入参。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModDetailQuery {
    pub source: ModSource,
    pub project_id: String,
    /// 用来判定 `mods/` 里是否已装、以及默认展开哪一组。
    #[serde(default)]
    pub root_id: Option<String>,
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub force: bool,
}

// ---------------------------------------------------------------------------
// 类型表 / 目标版本
// ---------------------------------------------------------------------------

/// 搜索卡里的类型选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModType {
    /// 稳定 id（= Modrinth slug 优先，缺 slug 时用 `cf<分类id>`）。
    pub id: String,
    pub label: String,
    /// 大类（界面里的分组标题）。
    pub group: String,
    /// CurseForge 分类 id；`None` 表示该源没有这个分类。
    pub cf_category_id: Option<i64>,
    /// Modrinth 分类 slug；空表示该源没有这个分类。
    pub modrinth_slugs: Vec<String>,
}

/// 当前选中版本的下载目标。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModTarget {
    pub root_id: String,
    pub version_id: String,
    /// 原版版本号（`1.21`、`26.3`）。
    pub game_version: String,
    pub loader: LoaderKind,
    pub loader_label: String,
    /// `mods/` 的绝对路径。
    pub mods_dir: String,
    pub isolated: bool,
    /// 例如「当前版本是原版，装上的模组不会被加载」。
    pub warning: Option<String>,
}

// ---------------------------------------------------------------------------
// 纯函数：版本区间 / 下载量 / 加载器标签 / 镜像地址
// ---------------------------------------------------------------------------

/// 把 `1.20.1` 归到 `1.20`、`26.3` 归到 `26.3`；快照与预发布返回 `None`。
pub fn game_family(version: &str) -> Option<String> {
    game_family_key(version).map(|(a, b)| format!("{a}.{b}"))
}

fn game_family_key(version: &str) -> Option<(u32, u32)> {
    let raw = version.trim();
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for part in &parts {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        nums.push(part.parse::<u32>().ok()?);
    }
    Some((nums[0], nums[1]))
}

/// 版本家族倒序列表（`26.3, 26.2, 1.21, …`），用于详情页筛选条。
pub fn sorted_game_families(versions: &[String]) -> Vec<String> {
    let mut keys: Vec<(u32, u32)> = versions.iter().filter_map(|v| game_family_key(v)).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter()
        .rev()
        .map(|(a, b)| format!("{a}.{b}"))
        .collect()
}

/// 版本区间文案：相邻家族合并成区间，覆盖到最新正式版时收敛成 `1.20+`。
pub fn format_game_range(versions: &[String], latest: Option<&str>) -> String {
    let mut keys: Vec<(u32, u32)> = versions.iter().filter_map(|v| game_family_key(v)).collect();
    keys.sort_unstable();
    keys.dedup();
    if keys.is_empty() {
        return "仅快照版本".to_string();
    }
    // 先合并成连续区间。
    let mut ranges: Vec<((u32, u32), (u32, u32))> = Vec::new();
    for key in keys {
        match ranges.last_mut() {
            Some((_, end)) if *end == key => {}
            Some((_, end)) if end.0 == key.0 && end.1 + 1 == key.1 => *end = key,
            _ => ranges.push((key, key)),
        }
    }
    let latest_key = latest.and_then(game_family_key);
    let count = ranges.len();
    let mut parts: Vec<String> = Vec::with_capacity(count);
    for (idx, (start, end)) in ranges.into_iter().enumerate() {
        let is_last = idx + 1 == count;
        if is_last && Some(end) == latest_key {
            parts.push(format!("{}+", family_label(start)));
        } else if start == end {
            parts.push(family_label(start));
        } else {
            parts.push(format!("{}-{}", family_label(start), family_label(end)));
        }
    }
    parts.join(", ")
}

fn family_label(key: (u32, u32)) -> String {
    format!("{}.{}", key.0, key.1)
}

/// 下载量文案：`1.99 亿` / `32 万` / `9876`。
pub fn format_downloads(count: u64) -> String {
    if count >= 100_000_000 {
        let text = format!("{:.2}", count as f64 / 100_000_000.0);
        let trimmed = text.trim_end_matches('0').trim_end_matches('.').to_string();
        format!("{trimmed} 亿")
    } else if count >= 10_000 {
        format!("{} 万", count / 10_000)
    } else {
        count.to_string()
    }
}

/// 规范加载器名（小写）；不认识的返回 `None`。
pub fn normalize_loader(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "fabric" => Some("fabric"),
        "neoforge" => Some("neoforge"),
        "forge" => Some("forge"),
        "quilt" => Some("quilt"),
        "liteloader" => Some("liteloader"),
        "rift" => Some("rift"),
        _ => None,
    }
}

/// 加载器中文/英文显示名。
pub fn loader_label(loader: &str) -> &'static str {
    match loader {
        "fabric" => "Fabric",
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "quilt" => "Quilt",
        "liteloader" => "LiteLoader",
        "rift" => "Rift",
        _ => "通用",
    }
}

/// 固定顺序拼加载器标签：`Fabric / NeoForge`。
pub fn join_loader_labels<'a>(loaders: impl IntoIterator<Item = &'a str>) -> String {
    // 按字母序：Fabric / Forge / NeoForge / Quilt。两个站的 `loaders` 数组顺序
    // 不固定（同一模组刷新两次都可能变），统一排序后才能稳定显示与测试。
    const ORDER: [&str; 6] = ["fabric", "forge", "neoforge", "quilt", "liteloader", "rift"];
    let mut present: Vec<&'static str> = Vec::new();
    for raw in loaders {
        if let Some(name) = normalize_loader(raw) {
            if !present.contains(&name) {
                present.push(name);
            }
        }
    }
    present.sort_by_key(|name| ORDER.iter().position(|o| o == name).unwrap_or(ORDER.len()));
    if present.is_empty() {
        return "通用".to_string();
    }
    present
        .into_iter()
        .map(loader_label)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 把官方地址映射到 MCIM 镜像；不认识的域名返回 `None`。
pub fn mirror_url_for(official: &str) -> Option<String> {
    const MAP: [(&str, &str); 7] = [
        (
            "https://staging-api.modrinth.com/",
            "https://mod.mcimirror.top/modrinth/",
        ),
        (
            "https://api.modrinth.com/",
            "https://mod.mcimirror.top/modrinth/",
        ),
        ("https://cdn.modrinth.com/", "https://mod.mcimirror.top/"),
        (
            "https://api.curseforge.com/",
            "https://mod.mcimirror.top/curseforge/",
        ),
        ("https://edge.forgecdn.net/", "https://mod.mcimirror.top/"),
        ("https://media.forgecdn.net/", "https://mod.mcimirror.top/"),
        (
            "https://mediafilez.forgecdn.net/",
            "https://mod.mcimirror.top/",
        ),
    ];
    for (from, to) in MAP {
        if let Some(rest) = official.strip_prefix(from) {
            return Some(format!("{to}{rest}"));
        }
    }
    None
}

/// 名称归一化：只留小写字母数字，用来做中文名词表匹配与已安装文件名比对。
pub fn normalize_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn game_family_accepts_release_versions_only() {
        assert_eq!(game_family("1.20.1").as_deref(), Some("1.20"));
        assert_eq!(game_family("1.21").as_deref(), Some("1.21"));
        assert_eq!(game_family("26.3").as_deref(), Some("26.3"));
        assert_eq!(game_family("26w14a"), None);
        assert_eq!(game_family("1.21-rc1"), None);
        assert_eq!(game_family("1.20.1-pre2"), None);
        assert_eq!(game_family(""), None);
    }

    #[test]
    fn game_range_merges_adjacent_families() {
        let versions = owned(&["1.20", "1.20.1", "1.21", "1.21.4"]);
        assert_eq!(format_game_range(&versions, None), "1.20-1.21");
    }

    #[test]
    fn game_range_keeps_gaps() {
        let versions = owned(&["1.16.5", "1.20.1", "26.2"]);
        assert_eq!(format_game_range(&versions, None), "1.16, 1.20, 26.2");
    }

    #[test]
    fn game_range_collapses_to_plus_when_reaching_latest() {
        let versions = owned(&["1.16.5", "1.20.1", "1.21.1"]);
        assert_eq!(format_game_range(&versions, Some("1.21.3")), "1.16, 1.20+");
        let versions = owned(&["1.20", "1.20.4", "1.21", "1.21.5", "26.2"]);
        assert_eq!(
            format_game_range(&versions, Some("26.2")),
            "1.20-1.21, 26.2+"
        );
    }

    #[test]
    fn game_range_ignores_snapshots() {
        let versions = owned(&["26w14a", "1.20.1-pre1"]);
        assert_eq!(format_game_range(&versions, None), "仅快照版本");
        let versions = owned(&["26w14a", "1.20.1"]);
        assert_eq!(format_game_range(&versions, None), "1.20");
    }

    #[test]
    fn sorted_families_are_descending_and_deduped() {
        let versions = owned(&["1.20.1", "26.2", "1.20", "26.3", "1.19.2", "24w14a"]);
        assert_eq!(
            sorted_game_families(&versions),
            owned(&["26.3", "26.2", "1.20", "1.19"])
        );
    }

    #[test]
    fn downloads_use_chinese_units() {
        assert_eq!(format_downloads(257_195_667), "2.57 亿");
        assert_eq!(format_downloads(199_000_000), "1.99 亿");
        assert_eq!(format_downloads(100_000_000), "1 亿");
        assert_eq!(format_downloads(32_145), "3 万");
        assert_eq!(format_downloads(9_999), "9999");
        assert_eq!(format_downloads(0), "0");
    }

    #[test]
    fn loader_labels_follow_fixed_order() {
        assert_eq!(
            join_loader_labels(["neoforge", "fabric"]),
            "Fabric / NeoForge"
        );
        assert_eq!(
            join_loader_labels(["quilt", "fabric", "forge"]),
            "Fabric / Forge / Quilt"
        );
        assert_eq!(join_loader_labels(Vec::<&str>::new()), "通用");
        assert_eq!(join_loader_labels(["datapack"]), "通用");
        assert_eq!(normalize_loader("NeoForge"), Some("neoforge"));
        assert_eq!(normalize_loader("iris"), None);
    }

    #[test]
    fn mirror_urls_cover_all_cdn_hosts() {
        assert_eq!(
            mirror_url_for("https://cdn.modrinth.com/data/AANobbMI/versions/abc/sodium.jar")
                .as_deref(),
            Some("https://mod.mcimirror.top/data/AANobbMI/versions/abc/sodium.jar")
        );
        assert_eq!(
            mirror_url_for("https://api.modrinth.com/v2/search?limit=20").as_deref(),
            Some("https://mod.mcimirror.top/modrinth/v2/search?limit=20")
        );
        assert_eq!(
            mirror_url_for("https://api.curseforge.com/v1/mods/search?gameId=432").as_deref(),
            Some("https://mod.mcimirror.top/curseforge/v1/mods/search?gameId=432")
        );
        assert_eq!(
            mirror_url_for("https://edge.forgecdn.net/files/5841/460/iris.jar").as_deref(),
            Some("https://mod.mcimirror.top/files/5841/460/iris.jar")
        );
        assert_eq!(
            mirror_url_for("https://mediafilez.forgecdn.net/files/5841/460/iris.jar").as_deref(),
            Some("https://mod.mcimirror.top/files/5841/460/iris.jar")
        );
        assert_eq!(mirror_url_for("https://example.invalid/x.jar"), None);
    }

    #[test]
    fn name_normalization_strips_punctuation() {
        assert_eq!(
            normalize_name("Reese's Sodium Options"),
            "reesessodiumoptions"
        );
        assert_eq!(normalize_name("AE2"), "ae2");
        assert_eq!(normalize_name("钠"), "");
    }

    #[test]
    fn mod_source_values_are_validated_and_legacy_ones_normalized() {
        assert!(is_valid_mod_source(MOD_SOURCE_OFFICIAL_FIRST));
        assert!(is_valid_mod_source(MOD_SOURCE_MIRROR_FIRST));
        assert!(is_valid_mod_source(MOD_SOURCE_OFFICIAL_ONLY));
        assert!(!is_valid_mod_source("mojang"));
        assert!(!is_valid_mod_source(""));

        // 旧配置里的三态要能平滑升级，不能变成「设置项不合法」。
        assert_eq!(normalize_mod_source("auto"), MOD_SOURCE_OFFICIAL_FIRST);
        assert_eq!(normalize_mod_source("mcim"), MOD_SOURCE_MIRROR_FIRST);
        assert_eq!(normalize_mod_source("official"), MOD_SOURCE_OFFICIAL_ONLY);
        assert_eq!(normalize_mod_source(""), MOD_SOURCE_OFFICIAL_FIRST);
        assert_eq!(
            normalize_mod_source(MOD_SOURCE_OFFICIAL_ONLY),
            MOD_SOURCE_OFFICIAL_ONLY
        );
    }

    #[test]
    fn description_mode_defaults_to_original() {
        assert!(is_valid_mod_description(MOD_DESCRIPTION_ORIGINAL));
        assert!(is_valid_mod_description(MOD_DESCRIPTION_TRANSLATED));
        assert!(!is_valid_mod_description("auto"));
        assert_eq!(normalize_mod_description(""), MOD_DESCRIPTION_ORIGINAL);
        assert_eq!(normalize_mod_description("乱写"), MOD_DESCRIPTION_ORIGINAL);
        assert_eq!(
            normalize_mod_description(MOD_DESCRIPTION_TRANSLATED),
            MOD_DESCRIPTION_TRANSLATED
        );
    }
}
