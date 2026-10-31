//! 模组中文名词表。
//!
//! MCIM 的翻译接口只译**简介**，不译名称；中文名来自我们自己的词表（PCL2 用的是
//! 同一思路：随启动器分发一份 mcmod.cn 词库）。这里做成三层：
//!
//! 1. **随包种子** `assets/mod-names-zh.seed.json`（`include_str!` 进二进制）；
//! 2. **数据目录覆盖** `<数据目录>/mod-names-zh.json`，同 key 覆盖种子、可只写新增项，
//!    所以扩库不需要重新发版；
//! 3. **离线扩库脚本** `scripts/mod-names/build-names.mjs`（开发工具，运行时零抓取）。
//!
//! 词表只存「中文名 ↔ 英文名/slug」对照，不存正文；数据来源与许可证见
//! `docs/superpowers/specs/` 下的模组下载小节。

use std::collections::HashMap;
use std::path::Path;

use kaleid_api::normalize_name;
use serde::{Deserialize, Serialize};

/// 数据目录里的覆盖词表文件名。
pub const USER_NAMES_FILE: &str = "mod-names-zh.json";

/// 随包种子词表。
pub const SEED_NAMES: &str = include_str!("../assets/mod-names-zh.seed.json");

/// 一条名称对照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameEntry {
    /// 中文名。
    pub zh: String,
    /// 英文原名。
    #[serde(default)]
    pub en: String,
    /// 站点 slug（Modrinth / CurseForge 同名）。
    #[serde(default)]
    pub slug: Option<String>,
    /// MC 百科页面 id；有的话「转到 MC 百科」可以直接打开条目。
    #[serde(default)]
    pub mcmod_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NameLibrary {
    #[serde(default)]
    entries: Vec<NameEntry>,
}

/// 载入后的词表（按英文名归一化、slug、中文名三索引）。
///
/// 中文索引是给「中文关键词 → 站点能搜的词」用的：Modrinth / CurseForge 只认
/// 英文名与 slug，用户直接搜「机械动力」是搜不到的，必须先查词表翻译。
#[derive(Debug, Default)]
pub struct ModNames {
    by_name: HashMap<String, NameEntry>,
    by_slug: HashMap<String, NameEntry>,
    by_zh: HashMap<String, NameEntry>,
}

impl ModNames {
    /// 先读种子，再用数据目录里的覆盖词表盖一遍。
    pub fn load(data_dir: Option<&Path>) -> Self {
        let mut names = ModNames::default();
        names.merge_json(SEED_NAMES);
        if let Some(dir) = data_dir {
            if let Ok(text) = std::fs::read_to_string(dir.join(USER_NAMES_FILE)) {
                names.merge_json(&text);
            }
        }
        names
    }

    /// 直接喂 JSON 文本（测试用）。
    pub fn from_json(text: &str) -> Self {
        let mut names = ModNames::default();
        names.merge_json(text);
        names
    }

    fn merge_json(&mut self, text: &str) {
        let Ok(library) = serde_json::from_str::<NameLibrary>(text) else {
            return;
        };
        for entry in library.entries {
            if entry.zh.trim().is_empty() {
                continue;
            }
            let key = normalize_name(&entry.en);
            if !key.is_empty() {
                self.by_name.insert(key, entry.clone());
            }
            if let Some(slug) = &entry.slug {
                let slug = slug.trim().to_ascii_lowercase();
                if !slug.is_empty() {
                    self.by_slug.insert(slug, entry.clone());
                }
            }
            // 中文名也建索引：`by_name` 只收 ASCII（`normalize_name` 会把汉字全丢掉），
            // 拿它给中文建键等于把所有中文名压成空串。
            let zh = zh_key(&entry.zh);
            if !zh.is_empty() {
                self.by_zh.insert(zh, entry.clone());
            }
        }
    }

    /// 条目数量（三个索引合并去重后的近似值，只用于日志/测试）。
    pub fn len(&self) -> usize {
        self.by_name
            .len()
            .max(self.by_slug.len())
            .max(self.by_zh.len())
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty() && self.by_slug.is_empty() && self.by_zh.is_empty()
    }

    /// 查中文名条目：先 slug 精确匹配，再英文名归一化匹配。
    pub fn lookup(&self, name: &str, slug: Option<&str>) -> Option<&NameEntry> {
        if let Some(slug) = slug {
            let slug = slug.trim().to_ascii_lowercase();
            if !slug.is_empty() {
                if let Some(hit) = self.by_slug.get(&slug) {
                    return Some(hit);
                }
            }
        }
        let key = normalize_name(name);
        if key.is_empty() {
            return None;
        }
        self.by_name.get(&key)
    }

    /// 只要中文名。
    pub fn translated(&self, name: &str, slug: Option<&str>) -> Option<String> {
        self.lookup(name, slug).map(|entry| entry.zh.clone())
    }

    /// 按**中文名**查条目。
    ///
    /// 这就是「用户输入中文关键词」那一侧的入口：搜「机械动力」时拿到的条目里
    /// 带着 slug 与英文名，调用方再用它去请求站点。查不到返回 `None`，调用方
    /// 应当原样使用用户输入，而不是报错。
    pub fn lookup_zh(&self, query: &str) -> Option<&NameEntry> {
        let key = zh_key(query);
        if key.is_empty() {
            return None;
        }
        self.by_zh.get(&key)
    }

    /// MC 百科页面：有 id 直连条目，否则退化成站内搜索。
    pub fn wiki_url(&self, name: &str, slug: Option<&str>) -> Option<String> {
        match self.lookup(name, slug).and_then(|e| e.mcmod_id) {
            Some(id) => Some(format!("https://www.mcmod.cn/class/{id}.html")),
            None => {
                // 没收录时退化成站内搜索（URL 编码按 UTF-8 字节走，中文名也能搜）。
                let mut query = String::new();
                for byte in name.trim().as_bytes() {
                    match byte {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                            query.push(*byte as char);
                        }
                        b' ' => query.push('+'),
                        other => query.push_str(&format!("%{other:02X}")),
                    }
                }
                if query.is_empty() {
                    None
                } else {
                    Some(format!("https://search.mcmod.cn/s?key={query}"))
                }
            }
        }
    }
}

/// 中文名归一化：只留字母数字（汉字属于 Alphabetic，会被保留），丢掉空格与
/// 全角/半角标点。
///
/// 不能用 [`normalize_name`]：那个只留 ASCII，中文名会被压成空串，整份中文
/// 索引就退化成一个键。
fn zh_key(raw: &str) -> String {
    raw.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_library_parses_and_has_entries() {
        let names = ModNames::load(None);
        assert!(names.len() > 20, "种子词表太小：{}", names.len());
        assert_eq!(
            names.translated("Sodium", Some("sodium")).as_deref(),
            Some("钠")
        );
        assert_eq!(
            names.translated("Create", None).as_deref(),
            Some("机械动力")
        );
    }

    #[test]
    fn lookup_is_case_and_punctuation_insensitive() {
        let names = ModNames::from_json(
            r#"{"entries":[{"zh":"苹果皮","en":"AppleSkin","slug":"appleskin"}]}"#,
        );
        assert_eq!(
            names.translated("appleskin", None).as_deref(),
            Some("苹果皮")
        );
        assert_eq!(
            names.translated("Apple Skin", None).as_deref(),
            Some("苹果皮")
        );
        // slug 优先，且不受大小写影响
        assert_eq!(
            names.translated("无关名字", Some("AppleSkin")).as_deref(),
            Some("苹果皮")
        );
        assert_eq!(names.translated("Unknown Mod", None), None);
        // 中文名归一化后为空，不能靠它命中
        assert_eq!(names.translated("苹果皮", None), None);
    }

    #[test]
    fn chinese_index_finds_entries_by_chinese_name() {
        let names = ModNames::from_json(
            r#"{"entries":[
                {"zh":"机械动力","en":"Create","slug":"create"},
                {"zh":"苹果皮","en":"AppleSkin","slug":"appleskin"}
            ]}"#,
        );
        // 完全命中中文名 → 能拿到 slug 与英文名，搜索翻译就靠它俩
        let hit = names.lookup_zh("机械动力").expect("中文名应当命中");
        assert_eq!(hit.en, "Create");
        assert_eq!(hit.slug.as_deref(), Some("create"));
        // 标点/空格/大小写不影响命中（用户会手打「苹果 皮」这种东西）
        assert_eq!(
            names.lookup_zh(" 苹果 皮 ").map(|e| e.en.clone()),
            Some("AppleSkin".to_string())
        );
        // 词表里没有的中文名返回 None：调用方退回原始 query，不报错
        assert!(names.lookup_zh("不存在的模组").is_none());
        // 空串/纯标点不能被当成"命中第一条"
        assert!(names.lookup_zh("").is_none());
        assert!(names.lookup_zh("  （） ").is_none());
    }

    #[test]
    fn user_library_overrides_seed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(USER_NAMES_FILE),
            r#"{"entries":[{"zh":"钠（覆盖）","en":"Sodium","slug":"sodium"},
                            {"zh":"新模组","en":"Brand New Mod","slug":"brand-new"}]}"#,
        )
        .unwrap();
        let names = ModNames::load(Some(dir.path()));
        assert_eq!(
            names.translated("Sodium", None).as_deref(),
            Some("钠（覆盖）")
        );
        assert_eq!(
            names.translated("Brand New Mod", None).as_deref(),
            Some("新模组")
        );
        // 种子里的其它条目仍在
        assert_eq!(
            names.translated("Create", None).as_deref(),
            Some("机械动力")
        );
    }

    #[test]
    fn broken_user_library_does_not_lose_seed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(USER_NAMES_FILE), "{not json").unwrap();
        let names = ModNames::load(Some(dir.path()));
        assert_eq!(
            names.translated("Create", None).as_deref(),
            Some("机械动力")
        );
    }

    #[test]
    fn wiki_url_prefers_class_page_then_search() {
        let names = ModNames::from_json(
            r#"{"entries":[{"zh":"钠","en":"Sodium","slug":"sodium","mcmodId":2785}]}"#,
        );
        assert_eq!(
            names.wiki_url("Sodium", None).as_deref(),
            Some("https://www.mcmod.cn/class/2785.html")
        );
        assert_eq!(
            names.wiki_url("Reese's Sodium Options", None).as_deref(),
            Some("https://search.mcmod.cn/s?key=Reese%27s+Sodium+Options")
        );
    }
}
