//! 模组下载：Modrinth + CurseForge 搜索、排行、详情与安装。
//!
//! 数据源策略（`GlobalSettings::mod_source`）：
//!
//! | 取值 | Modrinth | CurseForge |
//! |---|---|---|
//! | `auto` | 官方 → 失败回退 MCIM | MCIM（官方接口要 API Key） |
//! | `mcim` | 只用 MCIM | 只用 MCIM |
//! | `official` | 只用官方 | MCIM + 明确提示 |
//!
//! UA 必须是 MCIM 登记过的 `KMLH/{version_code}`，版本号来自仓库根 `build.env`
//! （见本 crate 的 `build.rs`）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use kaleid_api::{
    game_family, is_valid_mod_source, join_loader_labels, loader_label, mirror_url_for,
    normalize_loader, normalize_mod_description, normalize_mod_source, normalize_name,
    sorted_game_families, ApiError, ApiResult, DownloadStatus, InstalledMod, LoaderKind, ModDetail,
    ModDetailQuery, ModFile, ModHit, ModProject, ModSearchQuery, ModSearchResult, ModSource,
    ModSourceFilter, ModTarget, ModType, ModVersionGroup, RootRegistryService, SettingsServiceApi,
    VersionStoreService, MOD_DESCRIPTION_ORIGINAL, MOD_DESCRIPTION_TRANSLATED,
    MOD_SOURCE_MIRROR_FIRST, MOD_SOURCE_OFFICIAL_FIRST, MOD_SOURCE_OFFICIAL_ONLY,
};
use sha1::{Digest, Sha1};

use crate::downloads::{DownloadItem, DownloadManager, Job};
use crate::downloads::diagnostics::{Operation, Recorder, spawn_thread};
use kaleid_kernel::Context;
use crate::modnames::ModNames;
use crate::roots::RootRegistry;
use crate::settings::Settings;
use crate::versions::VersionStore;

/// MCIM 登记的启动器 UA：`KMLH/{version_code}`。
pub const USER_AGENT: &str = concat!("KMLH/", env!("KMLH_VERSION_CODE"));

pub(crate) const MODRINTH_OFFICIAL: &str = "https://api.modrinth.com/v2";
pub(crate) const MODRINTH_MIRROR: &str = "https://mod.mcimirror.top/modrinth/v2";
pub(crate) const CURSEFORGE_MIRROR: &str = "https://mod.mcimirror.top/curseforge/v1";
const MCIM_TRANSLATE: &str = "https://mod.mcimirror.top/translate";

/// CurseForge 的 Minecraft 游戏 id。
pub(crate) const CF_GAME_ID: i64 = 432;
/// CurseForge 的 Mod 分类 classId。
const CF_CLASS_MOD: i64 = 6;

const SEARCH_TTL: Duration = Duration::from_secs(300);
const PAGE_SIZE: u32 = 20;
/// 「热门」一页固定 50 个：这既是 Modrinth 的 `limit` 上限，也是 PCL2 的取法
/// （它一次只渲染一页，翻页再取下一页，而不是一次抓几百个）。
pub(crate) const RANKING_PAGE_SIZE: u32 = 50;
/// 「热门」缓存有效期：10 分钟。下载量排行是个位数分钟级才变的量，再短只是白费请求。
const RANKING_TTL: Duration = Duration::from_secs(600);

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 类型表（大类 → 子类；CF 只支持单个 categoryId，所以每条只带一个）
// ---------------------------------------------------------------------------

struct TypeDef {
    id: &'static str,
    label: &'static str,
    group: &'static str,
    cf: Option<i64>,
    mr: &'static [&'static str],
}

const TYPES: &[TypeDef] = &[
    // 世界与生成
    TypeDef {
        id: "worldgen",
        label: "世界生成",
        group: "世界与生成",
        cf: Some(406),
        mr: &["worldgen"],
    },
    TypeDef {
        id: "cf407",
        label: "生物群系",
        group: "世界与生成",
        cf: Some(407),
        mr: &[],
    },
    TypeDef {
        id: "cf408",
        label: "矿物与资源",
        group: "世界与生成",
        cf: Some(408),
        mr: &[],
    },
    TypeDef {
        id: "cf409",
        label: "结构",
        group: "世界与生成",
        cf: Some(409),
        mr: &[],
    },
    TypeDef {
        id: "cf410",
        label: "维度",
        group: "世界与生成",
        cf: Some(410),
        mr: &[],
    },
    TypeDef {
        id: "mobs",
        label: "生物",
        group: "世界与生成",
        cf: Some(411),
        mr: &["mobs"],
    },
    // 科技与工程
    TypeDef {
        id: "technology",
        label: "科技",
        group: "科技与工程",
        cf: Some(412),
        mr: &["technology"],
    },
    TypeDef {
        id: "cf4843",
        label: "自动化",
        group: "科技与工程",
        cf: Some(4843),
        mr: &[],
    },
    TypeDef {
        id: "cf413",
        label: "处理",
        group: "科技与工程",
        cf: Some(413),
        mr: &[],
    },
    TypeDef {
        id: "cf414",
        label: "玩家运输",
        group: "科技与工程",
        cf: Some(414),
        mr: &[],
    },
    TypeDef {
        id: "cf415",
        label: "管道物流",
        group: "科技与工程",
        cf: Some(415),
        mr: &[],
    },
    TypeDef {
        id: "cf416",
        label: "农业",
        group: "科技与工程",
        cf: Some(416),
        mr: &[],
    },
    TypeDef {
        id: "cf417",
        label: "能源",
        group: "科技与工程",
        cf: Some(417),
        mr: &[],
    },
    TypeDef {
        id: "cf418",
        label: "遗传",
        group: "科技与工程",
        cf: Some(418),
        mr: &[],
    },
    TypeDef {
        id: "cf4558",
        label: "红石",
        group: "科技与工程",
        cf: Some(4558),
        mr: &[],
    },
    TypeDef {
        id: "cf4570",
        label: "运输",
        group: "科技与工程",
        cf: Some(4570),
        mr: &["transportation"],
    },
    // 玩法与冒险
    TypeDef {
        id: "adventure",
        label: "冒险与 RPG",
        group: "玩法与冒险",
        cf: Some(422),
        mr: &["adventure"],
    },
    TypeDef {
        id: "magic",
        label: "魔法",
        group: "玩法与冒险",
        cf: Some(419),
        mr: &["magic"],
    },
    TypeDef {
        id: "food",
        label: "食物与烹饪",
        group: "玩法与冒险",
        cf: Some(436),
        mr: &["food"],
    },
    TypeDef {
        id: "minigame",
        label: "小游戏",
        group: "玩法与冒险",
        cf: None,
        mr: &["minigame"],
    },
    TypeDef {
        id: "economy",
        label: "经济",
        group: "玩法与冒险",
        cf: None,
        mr: &["economy"],
    },
    // 实用与信息
    TypeDef {
        id: "utility",
        label: "信息显示",
        group: "实用与信息",
        cf: Some(423),
        mr: &["utility"],
    },
    TypeDef {
        id: "storage",
        label: "仓储",
        group: "实用与信息",
        cf: Some(420),
        mr: &["storage"],
    },
    TypeDef {
        id: "cf435",
        label: "服务器",
        group: "实用与信息",
        cf: Some(435),
        mr: &[],
    },
    TypeDef {
        id: "cf5191",
        label: "改良",
        group: "实用与信息",
        cf: Some(5191),
        mr: &[],
    },
    TypeDef {
        id: "management",
        label: "管理",
        group: "实用与信息",
        cf: None,
        mr: &["management"],
    },
    TypeDef {
        id: "social",
        label: "社交",
        group: "实用与信息",
        cf: None,
        mr: &["social"],
    },
    // 优化与修复
    TypeDef {
        id: "optimization",
        label: "性能优化",
        group: "优化与修复",
        cf: Some(6814),
        mr: &["optimization"],
    },
    TypeDef {
        id: "cf6821",
        label: "修复",
        group: "优化与修复",
        cf: Some(6821),
        mr: &[],
    },
    // 前置与库
    TypeDef {
        id: "library",
        label: "支持库",
        group: "前置与库",
        cf: Some(421),
        mr: &["library"],
    },
    // 装饰与外观
    TypeDef {
        id: "equipment",
        label: "装备与工具",
        group: "装饰与外观",
        cf: Some(434),
        mr: &["equipment"],
    },
    TypeDef {
        id: "decoration",
        label: "装饰",
        group: "装饰与外观",
        cf: Some(424),
        mr: &["decoration"],
    },
    // 扩展与其他
    TypeDef {
        id: "cf426",
        label: "附属扩展",
        group: "扩展与其他",
        cf: Some(426),
        mr: &[],
    },
    TypeDef {
        id: "cf425",
        label: "杂项",
        group: "扩展与其他",
        cf: Some(425),
        mr: &[],
    },
    TypeDef {
        id: "cf5299",
        label: "教育",
        group: "扩展与其他",
        cf: Some(5299),
        mr: &[],
    },
    TypeDef {
        id: "cf10775",
        label: "恐怖",
        group: "扩展与其他",
        cf: Some(10775),
        mr: &[],
    },
    TypeDef {
        id: "cf4906",
        label: "创客",
        group: "扩展与其他",
        cf: Some(4906),
        mr: &[],
    },
    TypeDef {
        id: "cursed",
        label: "整活",
        group: "扩展与其他",
        cf: None,
        mr: &["cursed"],
    },
    TypeDef {
        id: "game-mechanics",
        label: "游戏机制",
        group: "扩展与其他",
        cf: None,
        mr: &["game-mechanics"],
    },
];

/// 搜索卡里的类型选项（按大类分组，界面自己排）。
pub fn mod_types() -> Vec<ModType> {
    TYPES
        .iter()
        .map(|t| ModType {
            id: t.id.to_string(),
            label: t.label.to_string(),
            group: t.group.to_string(),
            cf_category_id: t.cf,
            modrinth_slugs: t.mr.iter().map(|s| s.to_string()).collect(),
        })
        .collect()
}

fn type_by_id(id: &str) -> Option<&'static TypeDef> {
    TYPES.iter().find(|t| t.id == id)
}

fn types_for_mr(categories: &[String]) -> Vec<&'static TypeDef> {
    TYPES
        .iter()
        .filter(|t| t.mr.iter().any(|slug| categories.iter().any(|c| c == slug)))
        .collect()
}

fn types_for_cf(category_ids: &[i64]) -> Vec<&'static TypeDef> {
    TYPES
        .iter()
        .filter(|t| matches!(t.cf, Some(id) if category_ids.contains(&id)))
        .collect()
}

fn labels_of(defs: &[&'static TypeDef]) -> Vec<String> {
    defs.iter().map(|t| t.label.to_string()).collect()
}

fn primary_label(defs: &[&'static TypeDef]) -> String {
    defs.first()
        .map(|t| t.label.to_string())
        .unwrap_or_else(|| "其他".to_string())
}

// ---------------------------------------------------------------------------
// 缓存
// ---------------------------------------------------------------------------

struct TtlCache<T> {
    ttl: Duration,
    map: Mutex<HashMap<String, (Instant, T)>>,
}

impl<T: Clone> TtlCache<T> {
    fn new(ttl: Duration) -> Self {
        TtlCache {
            ttl,
            map: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, key: &str, force: bool) -> Option<T> {
        if force {
            return None;
        }
        let map = self.map.lock().unwrap();
        map.get(key).and_then(|(at, value)| {
            if at.elapsed() < self.ttl {
                Some(value.clone())
            } else {
                None
            }
        })
    }

    fn put(&self, key: String, value: T) {
        self.map
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), value));
    }
}

/// 一条「热门」缓存。
struct RankingEntry {
    /// 抓取时刻（epoch 毫秒）。存的是时间戳而不是 `Instant`：面板上要显示
    /// 「几分钟前更新」，而且换掉进程内的时间基准也不影响这份数据。
    fetched_at: u64,
    result: ModSearchResult,
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ModService {
    ctx: Recorder,
    settings: Arc<Settings>,
    roots: Arc<RootRegistry>,
    versions: Arc<VersionStore>,
    /// `version-manager` 被停用时为 `None`：还能搜索、看详情，只是不能下载。
    downloads: Option<Arc<DownloadManager>>,
    agent: ureq::Agent,
    names: Arc<ModNames>,
    /// 「热门」缓存：键 = 分面 + 游戏版本 + 加载器 + 页码 + 注释档位。
    ranking: Arc<Mutex<HashMap<String, RankingEntry>>>,
    search: Arc<TtlCache<ModSearchResult>>,
    detail: Arc<TtlCache<ModDetail>>,
    translate: Arc<Mutex<HashMap<String, Option<String>>>>,
}

impl ModService {
    pub fn new(
        settings: Arc<Settings>,
        roots: Arc<RootRegistry>,
        versions: Arc<VersionStore>,
        downloads: Option<Arc<DownloadManager>>,
        data_root: &Path,
    ) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build();
        ModService {
            ctx: None,
            settings,
            roots,
            versions,
            downloads,
            agent,
            names: Arc::new(ModNames::load(Some(data_root))),
            ranking: Arc::new(Mutex::new(HashMap::new())),
            search: Arc::new(TtlCache::new(SEARCH_TTL)),
            detail: Arc::new(TtlCache::new(SEARCH_TTL)),
            translate: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_context(mut self, ctx: Context) -> Self { self.ctx = Some(ctx); self }

    /// 当前数据源设置；老配置文件里的三态取值会被归一化成新取值。
    fn policy(&self) -> String {
        mod_policy(&self.settings)
    }

    /// 是否允许向 MCIM 的翻译接口请求简介译文。
    ///
    /// 这是**独立设置**（`modDescription`）：选译文时不论数据源选了什么都会去请求。
    fn allow_translate(&self) -> bool {
        self.description_mode() == MOD_DESCRIPTION_TRANSLATED
    }

    /// 当前「Mod 列表注释」档位（`original` / `translated`）。
    fn description_mode(&self) -> String {
        self.settings
            .global()
            .map(|g| normalize_mod_description(&g.mod_description))
            .unwrap_or_else(|_| MOD_DESCRIPTION_ORIGINAL.to_string())
    }

    fn push_warning(warnings: &mut Vec<String>, message: impl Into<String>) {
        push_warning(warnings, message);
    }

    /// 按策略拼出这条请求的候选 URL（官方 / 镜像）。
    fn candidate_urls(
        &self,
        source: ModSource,
        path: &str,
        warnings: &mut Vec<String>,
    ) -> Vec<String> {
        let policy = self.policy();
        // 官方优先下走镜像是**预期行为**，不该每次都弹一条黄条；只有用户
        // 明确选了「只用官方源」时才提示我们没照办（原因见设置里的说明）。
        if source == ModSource::CurseForge && policy == MOD_SOURCE_OFFICIAL_ONLY {
            Self::push_warning(warnings, "CurseForge 官方接口需要 API Key，已使用 MCIM 镜像");
        }
        candidate_urls(&policy, source, path)
    }

    fn fetch(&self, source: ModSource, path: &str, warnings: &mut Vec<String>) -> ApiResult<Value> {
        let urls = self.candidate_urls(source, path, warnings);
        let mut last_error = String::new();
        for (index, url) in urls.iter().enumerate() {
            match self.get_json(url) {
                Ok(value) => {
                    if index > 0 {
                        Self::push_warning(warnings, "官方接口不可用，已改用 MCIM 镜像");
                    }
                    return Ok(value);
                }
                Err(err) => last_error = err,
            }
        }
        Err(ApiError::Network(format!(
            "{} 请求失败：{last_error}",
            source.label()
        )))
    }

    fn get_json(&self, url: &str) -> Result<Value, String> {
        for attempt in 0..2u8 {
            match self.agent.get(url).call() {
                Ok(response) => {
                    return response
                        .into_json::<Value>()
                        .map_err(|e| format!("响应不是 JSON：{e}"));
                }
                Err(ureq::Error::Status(429, _)) if attempt == 0 => {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(ureq::Error::Status(code, response)) => {
                    let body = response.into_string().unwrap_or_default();
                    let short: String = body.trim().chars().take(160).collect();
                    return Err(if code == 404 {
                        format!("HTTP 404（镜像可能尚未缓存该条目）：{short}")
                    } else {
                        format!("HTTP {code}：{short}")
                    });
                }
                Err(err) => return Err(err.to_string()),
            }
        }
        Err("请求被限流（HTTP 429）".into())
    }

    fn post_json(&self, url: &str, body: Value) -> Result<Value, String> {
        let response = self
            .agent
            .post(url)
            .send_json(body)
            .map_err(|e| e.to_string())?;
        response
            .into_json::<Value>()
            .map_err(|e| format!("响应不是 JSON：{e}"))
    }

    // -- 排行榜 ------------------------------------------------------------

    /// 「热门」：照 PCL2 的取法，**按当前版本 + 加载器 + 类型分面，一次只取一页 50 个**。
    ///
    /// 排序用 `index=downloads`（PCL2 的「热门」就是下载量），不用 `relevance`：
    /// 这里没有搜索词，`relevance` 只会退化成默认排序。
    ///
    /// 为什么只查 Modrinth：分面是 Modrinth 专有的语法，CurseForge 的搜索接口
    /// 表达不了"这个游戏版本 + 这个加载器 + 这个分类"，把两边的结果混进同一页
    /// 会让用户看到一堆在自己版本上装不上的模组。
    pub fn ranking(&self, query: ModRankingQuery) -> ApiResult<ModSearchResult> {
        let mode = self.description_mode();
        let game = query.game_version.as_deref().unwrap_or("").trim().to_string();
        let loader = query.loader.as_deref().unwrap_or("").trim().to_string();
        let page = query.page.unwrap_or(0);
        let key = ranking_cache_key(&game, &loader, query.type_id.as_deref(), &mode, page);
        if !query.force {
            if let Some(cached) = self.cached_ranking(&key) {
                return Ok(cached);
            }
        }

        let mut warnings = Vec::new();
        let facets = ranking_facets(&game, &loader, query.type_id.as_deref());
        let offset = page.saturating_mul(RANKING_PAGE_SIZE);
        let path = format!(
            "/search?limit={RANKING_PAGE_SIZE}&offset={offset}&index=downloads&facets={}",
            percent_encode(&facets)
        );
        let value = self.fetch(ModSource::Modrinth, &path, &mut warnings)?;
        let mr_total = value
            .get("total_hits")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let latest = if looks_like_mc_version(&game) {
            Some(game.as_str())
        } else {
            None
        };
        let mut hits = parse_mr_hits(&value, latest);
        if hits.is_empty() {
            // 分组筛选得很窄时"一条都没有"是正常结果，不是错误：界面已经有
            // 「暂时没有排行数据」的空状态，这里只补一句为什么。
            let has_filter = !facets.trim().eq(r#"[["project_type:mod"]]"#);
            Self::push_warning(&mut warnings, ranking_empty_warning(has_filter));
        }
        hits.sort_by(|a, b| b.downloads.cmp(&a.downloads));
        for hit in &mut hits {
            self.decorate_names(hit);
        }
        self.translate_hits(&mut hits, &mut warnings);
        let result = ModSearchResult {
            hits,
            modrinth_total: mr_total,
            // 这一页只有 Modrinth 的条目，CF 的条目数没有意义，别让界面拿它算页数。
            curseforge_total: 0,
            warnings,
            fetched_at: now_ms(),
        };
        self.store_ranking(&key, result.clone());
        Ok(result)
    }

    /// 取缓存：同一组参数在 10 分钟内只请求一次。
    fn cached_ranking(&self, key: &str) -> Option<ModSearchResult> {
        let map = self.ranking.lock().unwrap();
        let entry = map.get(key)?;
        let age = now_ms().saturating_sub(entry.fetched_at);
        if age < RANKING_TTL.as_millis() as u64 {
            Some(entry.result.clone())
        } else {
            None
        }
    }

    fn store_ranking(&self, key: &str, result: ModSearchResult) {
        let mut map = self.ranking.lock().unwrap();
        // 翻页会攒出很多键：顺手把过期的清掉，别让这个 map 无限长。
        let ttl = RANKING_TTL.as_millis() as u64;
        let now = now_ms();
        map.retain(|_, entry| now.saturating_sub(entry.fetched_at) < ttl);
        map.insert(
            key.to_string(),
            RankingEntry {
                fetched_at: result.fetched_at,
                result,
            },
        );
    }

    // -- 搜索 --------------------------------------------------------------

    pub fn search(&self, query: ModSearchQuery) -> ApiResult<ModSearchResult> {
        let keyword = query.query.trim().to_string();
        // 两个站点都只认英文名与 slug：中文关键词先按名称表翻一次。翻不出来就
        // 原样发出去——"搜不到"是搜索结果本身要表达的事，不该升级成一个错误。
        let remote_keyword = if is_zh(&keyword) {
            translate_query(&self.names, &keyword).unwrap_or_else(|| keyword.clone())
        } else {
            keyword.clone()
        };
        let type_id = query.type_id.clone().unwrap_or_default();
        let page = query.page.unwrap_or(0);
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            keyword,
            source_filter_key(query.source),
            type_id,
            query.latest_game_version.clone().unwrap_or_default(),
            // 注释档位进缓存键：切到「翻译」后不能拿旧的无译文缓存糊弄。
            self.description_mode(),
            page,
        );
        if let Some(cached) = self.search.get(&key, query.force) {
            return Ok(cached);
        }

        let mut warnings = Vec::new();
        let mut hits: Vec<ModHit> = Vec::new();
        let mut mr_total = 0i64;
        let mut cf_total = 0i64;
        let latest = query.latest_game_version.as_deref();
        let type_def = type_by_id(&type_id);
        let limit = query.page_size.unwrap_or(PAGE_SIZE).clamp(1, 50);
        // 翻页偏移：Modrinth 用 `offset`，CurseForge 用 `index`，语义相同。
        // 两源**同一页同一个偏移**，界面上"第 N 页"对两边是一致的。
        let offset = page.saturating_mul(limit);

        // Modrinth
        if query.source.includes(ModSource::Modrinth) {
            let supports = type_def.map(|t| !t.mr.is_empty()).unwrap_or(true);
            if !supports {
                let label = type_def.map(|t| t.label).unwrap_or("该类型");
                Self::push_warning(
                    &mut warnings,
                    format!("Modrinth 没有「{label}」分类，已只搜索 CurseForge"),
                );
            } else {
                let mut facet_groups: Vec<String> = vec![r#"["project_type:mod"]"#.into()];
                if let Some(def) = type_def {
                    if let Some(slug) = def.mr.first() {
                        facet_groups.push(format!(r#"["categories:{slug}"]"#));
                    }
                }
                let facets = percent_encode(&format!("[{}]", facet_groups.join(",")));
                let index = if remote_keyword.is_empty() {
                    "downloads"
                } else {
                    "relevance"
                };
                let mut path =
                    format!("/search?limit={limit}&offset={offset}&index={index}&facets={facets}");
                if !remote_keyword.is_empty() {
                    path.push_str(&format!("&query={}", percent_encode(&remote_keyword)));
                }
                match self.fetch(ModSource::Modrinth, &path, &mut warnings) {
                    Ok(value) => {
                        mr_total = value
                            .get("total_hits")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        hits.extend(parse_mr_hits(&value, latest));
                    }
                    Err(err) => Self::push_warning(&mut warnings, err.to_string()),
                }
            }
        }

        // CurseForge
        if query.source.includes(ModSource::CurseForge) {
            let supports = type_def.map(|t| t.cf.is_some()).unwrap_or(true);
            if !supports {
                let label = type_def.map(|t| t.label).unwrap_or("该类型");
                Self::push_warning(
                    &mut warnings,
                    format!("CurseForge 没有「{label}」分类，已只搜索 Modrinth"),
                );
            } else {
                let mut path = format!(
                    "/mods/search?gameId={CF_GAME_ID}&classId={CF_CLASS_MOD}&pageSize={limit}&index={offset}"
                );
                // `sortField=6` 是「总下载量」。有搜索词时也**必须**带上它：实测
                // 只给 `searchFilter` 时 CF 会按自己的 featured 排序返回一堆不相干
                // 的模组（搜 sodium 能返回完全无关的东西）。
                path.push_str("&sortField=6&sortOrder=desc");
                if !remote_keyword.is_empty() {
                    path.push_str(&format!(
                        "&searchFilter={}",
                        percent_encode(&remote_keyword)
                    ));
                }
                if let Some(def) = type_def {
                    if let Some(id) = def.cf {
                        path.push_str(&format!("&categoryId={id}"));
                    }
                }
                match self.fetch(ModSource::CurseForge, &path, &mut warnings) {
                    Ok(value) => {
                        cf_total = value
                            .get("pagination")
                            .and_then(|p| p.get("totalCount"))
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        hits.extend(parse_cf_hits(&value, latest));
                    }
                    Err(err) => Self::push_warning(&mut warnings, err.to_string()),
                }
            }
        }

        if hits.is_empty() && !warnings.is_empty() && mr_total == 0 && cf_total == 0 {
            if let Some(first) = warnings.first().cloned() {
                if first.contains("请求失败") {
                    return Err(ApiError::Network(first));
                }
            }
        }
        for hit in &mut hits {
            self.decorate_names(hit);
        }
        self.translate_hits(&mut hits, &mut warnings);
        let result = ModSearchResult {
            hits,
            modrinth_total: mr_total,
            curseforge_total: cf_total,
            warnings,
            fetched_at: now_ms(),
        };
        self.search.put(key, result.clone());
        Ok(result)
    }

    // -- 目标版本 ----------------------------------------------------------

    pub fn target(&self, root_id: &str, version_id: &str) -> ApiResult<ModTarget> {
        let root = self.roots.get(root_id)?;
        let json_path = root
            .versions_dir()
            .join(version_id)
            .join(format!("{version_id}.json"));
        let summary = self
            .versions
            .inspect(&json_path)
            .map_err(|_| ApiError::not_found(format!("读不出版本 {version_id} 的 JSON")))?;
        let game_version = self.resolve_game_version(&root, &summary);
        let isolated = self
            .settings
            .resolve(root_id, version_id, root.isolation)?
            .isolated;
        let mods_dir = root.game_dir(version_id, isolated).join("mods");
        let warning = match summary.loader {
            LoaderKind::Vanilla => Some("当前版本是原版，装上的模组不会被加载".to_string()),
            LoaderKind::Unrecognized => {
                Some("没能识别这个版本的加载器，模组可能不会被加载".to_string())
            }
            _ => None,
        };
        Ok(ModTarget {
            root_id: root.id.clone(),
            version_id: version_id.to_string(),
            game_version,
            loader: summary.loader,
            loader_label: summary.loader.label_zh().to_string(),
            mods_dir: mods_dir.to_string_lossy().to_string(),
            isolated,
            warning,
        })
    }

    /// 沿 `inheritsFrom` 上溯到原版那一层，拿真正的游戏版本号。
    fn resolve_game_version(
        &self,
        root: &kaleid_api::Root,
        summary: &kaleid_api::VersionSummary,
    ) -> String {
        resolve_game_version(self.versions.as_ref(), root, summary)
    }

    // -- 详情 --------------------------------------------------------------

    pub fn detail(&self, query: ModDetailQuery) -> ApiResult<ModDetail> {
        let key = format!("{}:{}", query.source.as_str(), query.project_id);
        if let Some(cached) = self.detail.get(&key, query.force) {
            return Ok(self.with_installed(cached, &query));
        }
        let mut warnings = Vec::new();
        let mut detail = match query.source {
            ModSource::Modrinth => self.modrinth_detail(&query.project_id, &mut warnings)?,
            ModSource::CurseForge => self.curseforge_detail(&query.project_id, &mut warnings)?,
        };
        detail.warnings = warnings;
        if self.allow_translate() {
            if let Some(translated) = self.translate_description(query.source, &query.project_id) {
                detail.project.translated_description = Some(translated);
            }
        }
        self.detail.put(key, detail.clone());
        Ok(self.with_installed(detail, &query))
    }

    /// 详情缓存是「与安装目录无关」的，安装状态每次现算。
    fn with_installed(&self, mut detail: ModDetail, query: &ModDetailQuery) -> ModDetail {
        detail.project.installed = None;
        let (Some(root_id), Some(version_id)) =
            (query.root_id.as_deref(), query.version_id.as_deref())
        else {
            return detail;
        };
        let Ok(target) = self.target(root_id, version_id) else {
            return detail;
        };
        let known: HashSet<String> = detail
            .groups
            .iter()
            .flat_map(|g| g.files.iter())
            .map(|f| f.file_name.to_lowercase())
            .collect();
        detail.project.installed = detect_installed(
            Path::new(&target.mods_dir),
            &detail.project.slug,
            &detail.project.name,
            &known,
        );
        detail
    }

    fn modrinth_detail(
        &self,
        project_id: &str,
        warnings: &mut Vec<String>,
    ) -> ApiResult<ModDetail> {
        let project = self.fetch(
            ModSource::Modrinth,
            &format!("/project/{project_id}"),
            warnings,
        )?;
        let versions = self.fetch(
            ModSource::Modrinth,
            &format!("/project/{project_id}/version"),
            warnings,
        )?;
        let list = versions.as_array().cloned().unwrap_or_default();

        let mut groups: HashMap<(String, String), ModVersionGroup> = HashMap::new();
        // 每个版本文件自己的前置（必选, 可选），装配到组时再按组内文件求并集。
        let mut file_deps: HashMap<String, (HashSet<String>, HashSet<String>)> = HashMap::new();
        let mut all_versions: Vec<String> = Vec::new();

        for item in &list {
            let version_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let version_number = item
                .get("version_number")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let display_name = item
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| version_number.clone());
            let published = item
                .get("date_published")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let downloads = item.get("downloads").and_then(|v| v.as_u64()).unwrap_or(0);
            let release_type = item
                .get("version_type")
                .and_then(|v| v.as_str())
                .unwrap_or("release")
                .to_string();
            let game_versions = strings_of(item.get("game_versions"));
            all_versions.extend(game_versions.iter().cloned());
            let loaders: Vec<String> = strings_of(item.get("loaders"))
                .iter()
                .filter_map(|l| normalize_loader(l).map(|s| s.to_string()))
                .collect();
            let Some(file) = primary_file(item) else {
                continue;
            };
            let file_name = file
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if file_name.is_empty() || version_id.is_empty() {
                continue;
            }
            let official_url = file
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mirror = mirror_url_for(&official_url);
            let size = file.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            let sha1 = file
                .get("hashes")
                .and_then(|h| h.get("sha1"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let mut required = 0usize;
            let mut req_ids: HashSet<String> = HashSet::new();
            let mut opt_ids: HashSet<String> = HashSet::new();
            if let Some(deps) = item.get("dependencies").and_then(|v| v.as_array()) {
                for dep in deps {
                    let kind = dep
                        .get("dependency_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let Some(dep_id) = dep.get("project_id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    match kind {
                        "required" => {
                            required += 1;
                            req_ids.insert(dep_id.to_string());
                        }
                        "optional" | "embedded" => {
                            opt_ids.insert(dep_id.to_string());
                        }
                        _ => {}
                    }
                }
            }
            file_deps.insert(version_id.to_string(), (req_ids, opt_ids));

            let families: Vec<String> = game_versions
                .iter()
                .filter_map(|v| game_family(v))
                .collect();
            let loader_list = if loaders.is_empty() {
                vec!["universal".to_string()]
            } else {
                loaders.clone()
            };
            for loader in &loader_list {
                for family in &families {
                    let key = (loader.clone(), family.clone());
                    let group = groups.entry(key).or_insert_with(|| ModVersionGroup {
                        loader: loader.clone(),
                        loader_label: loader_label(loader).to_string(),
                        game_version: family.clone(),
                        dependencies: Vec::new(),
                        optional_dependencies: Vec::new(),
                        files: Vec::new(),
                    });
                    if group
                        .files
                        .iter()
                        .any(|f| f.file_id == version_id && f.file_name == file_name)
                    {
                        continue;
                    }
                    group.files.push(ModFile {
                        source: ModSource::Modrinth,
                        project_id: project_id.to_string(),
                        file_id: version_id.to_string(),
                        name: display_name.clone(),
                        file_name: file_name.clone(),
                        version_number: version_number.clone(),
                        size,
                        downloads,
                        published_at: published.clone(),
                        release_type: release_type.clone(),
                        loaders: loader_list.clone(),
                        game_versions: game_versions.clone(),
                        official_url: official_url.clone(),
                        mirror_url: mirror.clone(),
                        sha1: sha1.clone(),
                        required_deps: required,
                    });
                }
            }
        }

        let mut detail = self.assemble_detail(
            build_project_from_modrinth(&project, &all_versions),
            groups,
            all_versions,
            ModSource::Modrinth,
            warnings,
        );
        self.resolve_dependencies(ModSource::Modrinth, &file_deps, &mut detail, warnings);
        Ok(detail)
    }

    fn curseforge_detail(
        &self,
        project_id: &str,
        warnings: &mut Vec<String>,
    ) -> ApiResult<ModDetail> {
        let mod_json = self.fetch(
            ModSource::CurseForge,
            &format!("/mods/{project_id}"),
            warnings,
        )?;
        let files_json = self.fetch(
            ModSource::CurseForge,
            &format!("/mods/{project_id}/files?pageSize=50"),
            warnings,
        )?;
        let mod_data = mod_json.get("data").cloned().unwrap_or(Value::Null);
        let files = files_json
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut groups: HashMap<(String, String), ModVersionGroup> = HashMap::new();
        let mut file_deps: HashMap<String, (HashSet<String>, HashSet<String>)> = HashMap::new();
        let mut all_versions: Vec<String> = Vec::new();

        for file in &files {
            let file_id = file
                .get("id")
                .and_then(|v| v.as_i64())
                .map(|v| v.to_string())
                .unwrap_or_default();
            let file_name = file
                .get("fileName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if file_id.is_empty() || file_name.is_empty() {
                continue;
            }
            // CF 的 `gameVersions` 把加载器和 `Client` / `Server` 这类标签混在
            // 一起（例如 `["26.3", "Client", "Fabric", "Server"]`），所以先拆开：
            // 加载器走 normalize_loader，版本只留能认出家族号的。
            let raw_versions = strings_of(file.get("gameVersions"));
            let game_versions: Vec<String> = raw_versions
                .iter()
                .filter(|v| game_family(v).is_some())
                .cloned()
                .collect();
            all_versions.extend(game_versions.iter().cloned());
            let loaders: Vec<String> = raw_versions
                .iter()
                .filter_map(|v| normalize_loader(v).map(|s| s.to_string()))
                .collect();
            let families: Vec<String> = game_versions
                .iter()
                .filter_map(|v| game_family(v))
                .collect();
            let official_url = file
                .get("downloadUrl")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let official_url = if official_url.is_empty() {
                self.cf_download_url(project_id, &file_id, warnings)
                    .unwrap_or_default()
            } else {
                official_url
            };
            let mirror = mirror_url_for(&official_url);
            let size = file.get("fileLength").and_then(|v| v.as_u64()).unwrap_or(0);
            let downloads = file
                .get("downloadCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let published = file
                .get("fileDate")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let release_type = match file.get("releaseType").and_then(|v| v.as_i64()) {
                Some(2) => "beta",
                Some(3) => "alpha",
                _ => "release",
            }
            .to_string();
            let sha1 = file
                .get("hashes")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|h| h.get("algo").and_then(|a| a.as_i64()) == Some(1))
                })
                .and_then(|h| h.get("value"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let display_name = file
                .get("displayName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| file_name.clone());

            let mut required = 0usize;
            let mut req_ids: HashSet<String> = HashSet::new();
            let mut opt_ids: HashSet<String> = HashSet::new();
            if let Some(deps) = file.get("dependencies").and_then(|v| v.as_array()) {
                for dep in deps {
                    let Some(dep_id) = dep.get("modId").and_then(|v| v.as_i64()) else {
                        continue;
                    };
                    match dep.get("relationType").and_then(|v| v.as_i64()) {
                        Some(3) => {
                            required += 1;
                            req_ids.insert(dep_id.to_string());
                        }
                        Some(2) => {
                            opt_ids.insert(dep_id.to_string());
                        }
                        _ => {}
                    }
                }
            }
            file_deps.insert(file_id.clone(), (req_ids, opt_ids));

            let loader_list = if loaders.is_empty() {
                vec!["universal".to_string()]
            } else {
                loaders.clone()
            };
            for loader in &loader_list {
                for family in &families {
                    let key = (loader.clone(), family.clone());
                    let group = groups.entry(key).or_insert_with(|| ModVersionGroup {
                        loader: loader.clone(),
                        loader_label: loader_label(loader).to_string(),
                        game_version: family.clone(),
                        dependencies: Vec::new(),
                        optional_dependencies: Vec::new(),
                        files: Vec::new(),
                    });
                    if group
                        .files
                        .iter()
                        .any(|f| f.file_id == file_id && f.file_name == file_name)
                    {
                        continue;
                    }
                    group.files.push(ModFile {
                        source: ModSource::CurseForge,
                        project_id: project_id.to_string(),
                        file_id: file_id.clone(),
                        name: display_name.clone(),
                        file_name: file_name.clone(),
                        version_number: file_name.clone(),
                        size,
                        downloads,
                        published_at: published.clone(),
                        release_type: release_type.clone(),
                        loaders: loader_list.clone(),
                        game_versions: game_versions.clone(),
                        official_url: official_url.clone(),
                        mirror_url: mirror.clone(),
                        sha1: sha1.clone(),
                        required_deps: required,
                    });
                }
            }
        }

        let mut detail = self.assemble_detail(
            build_project_from_curseforge(&mod_data, &all_versions),
            groups,
            all_versions,
            ModSource::CurseForge,
            warnings,
        );
        self.resolve_dependencies(ModSource::CurseForge, &file_deps, &mut detail, warnings);
        Ok(detail)
    }

    fn cf_download_url(
        &self,
        mod_id: &str,
        file_id: &str,
        warnings: &mut Vec<String>,
    ) -> Option<String> {
        let value = self
            .fetch(
                ModSource::CurseForge,
                &format!("/mods/{mod_id}/files/{file_id}/download-url"),
                warnings,
            )
            .ok()?;
        value
            .get("data")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn assemble_detail(
        &self,
        project: ModProject,
        groups: HashMap<(String, String), ModVersionGroup>,
        all_versions: Vec<String>,
        _source: ModSource,
        _warnings: &mut Vec<String>,
    ) -> ModDetail {
        let mut groups: Vec<ModVersionGroup> = groups.into_values().collect();
        for group in &mut groups {
            group
                .files
                .sort_by(|a, b| b.published_at.cmp(&a.published_at));
        }
        groups.sort_by(|a, b| {
            version_key(&b.game_version)
                .cmp(&version_key(&a.game_version))
                .then_with(|| a.loader_label.cmp(&b.loader_label))
        });
        let versions = sorted_game_families(&all_versions);
        ModDetail {
            project,
            groups,
            versions,
            warnings: Vec::new(),
        }
    }

    /// 把前置 id 批量补成列表项；解析失败只提示，不阻塞详情。
    ///
    /// `file_deps` 是「文件 id → (必选前置, 可选前置)」：组的排布是
    /// 「加载器 + MC 版本」，同一组里可能有多个文件，所以按组内文件求并集，
    /// 而不是把整个项目的前置平摊到每一组。
    fn resolve_dependencies(
        &self,
        source: ModSource,
        file_deps: &HashMap<String, (HashSet<String>, HashSet<String>)>,
        detail: &mut ModDetail,
        warnings: &mut Vec<String>,
    ) {
        let mut all_required: HashSet<String> = HashSet::new();
        let mut all_optional: HashSet<String> = HashSet::new();
        for (required, optional) in file_deps.values() {
            all_required.extend(required.iter().cloned());
            all_optional.extend(optional.iter().cloned());
        }
        let required_hits = self.resolve_hits(source, &all_required, warnings);
        let optional_hits = self.resolve_hits(source, &all_optional, warnings);
        let missing = all_required.len().saturating_sub(required_hits.len());
        if missing > 0 {
            Self::push_warning(
                warnings,
                format!("有 {missing} 个前置资源没能解析，可以直接下载或手动补齐"),
            );
        }
        for group in &mut detail.groups {
            let mut required: HashSet<String> = HashSet::new();
            let mut optional: HashSet<String> = HashSet::new();
            for file in &group.files {
                if let Some((req, opt)) = file_deps.get(&file.file_id) {
                    required.extend(req.iter().cloned());
                    optional.extend(opt.iter().cloned());
                }
            }
            group.dependencies = required_hits
                .iter()
                .filter(|hit| required.contains(&hit.id))
                .cloned()
                .collect();
            group.optional_dependencies = optional_hits
                .iter()
                .filter(|hit| optional.contains(&hit.id))
                .cloned()
                .collect();
        }
    }

    fn resolve_hits(
        &self,
        source: ModSource,
        ids: &HashSet<String>,
        warnings: &mut Vec<String>,
    ) -> Vec<ModHit> {
        if ids.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let list: Vec<String> = ids.iter().cloned().collect();
        match source {
            ModSource::Modrinth => {
                let ids_json = json!(list);
                let path = format!("/projects?ids={}", percent_encode(&ids_json.to_string()));
                if let Ok(value) = self.fetch(ModSource::Modrinth, &path, warnings) {
                    for item in value.as_array().cloned().unwrap_or_default() {
                        if let Some(hit) = parse_mr_project_hit(&item) {
                            out.push(self.decorate(hit));
                        }
                    }
                }
            }
            ModSource::CurseForge => {
                let mod_ids: Vec<i64> = list
                    .iter()
                    .filter_map(|id| id.parse::<i64>().ok())
                    .collect();
                if mod_ids.is_empty() {
                    return out;
                }
                let url = format!("{CURSEFORGE_MIRROR}/mods");
                if let Ok(value) = self.post_json(&url, json!({ "modIds": mod_ids })) {
                    for item in value
                        .get("data")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default()
                    {
                        if let Some(hit) = parse_cf_item_hit(&item, None) {
                            out.push(self.decorate(hit));
                        }
                    }
                }
            }
        }
        out
    }

    // -- 译文 / 名称 -------------------------------------------------------

    fn translate_description(&self, source: ModSource, id: &str) -> Option<String> {
        let key = format!("{}:{id}", source.as_str());
        if let Some(cached) = self.translate.lock().unwrap().get(&key).cloned() {
            return cached;
        }
        let path = match source {
            ModSource::Modrinth => format!("{MCIM_TRANSLATE}/modrinth/{id}"),
            ModSource::CurseForge => format!("{MCIM_TRANSLATE}/curseforge/{id}"),
        };
        let translated = self.get_json(&path).ok().and_then(|value| {
            value
                .get("translated")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        self.translate
            .lock()
            .unwrap()
            .insert(key, translated.clone());
        translated
    }

    fn decorate_names(&self, hit: &mut ModHit) {
        hit.translated_name = self.names.translated(&hit.name, Some(&hit.slug));
        hit.wiki_url = self.names.wiki_url(&hit.name, Some(&hit.slug));
    }

    /// 给整页结果补上简介译文（只在「注释 = 翻译」时跑）。
    ///
    /// 列表一次 20–40 条，逐条串行要好几秒，所以用固定几个线程并发请求；MCIM 那边
    /// 有缓存，重复的条目在 `translate_description` 里直接命中内存缓存。
    /// 拿不到译文的条目就保持原文——绝不因为翻译失败让整页搜索失败。
    fn translate_hits(&self, hits: &mut [ModHit], warnings: &mut Vec<String>) {
        if !self.allow_translate() || hits.is_empty() {
            return;
        }
        const WORKERS: usize = 6;
        let pending: Vec<(usize, ModSource, String)> = hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.translated_description.is_none())
            .map(|(index, hit)| (index, hit.source, hit.id.clone()))
            .collect();
        if pending.is_empty() {
            return;
        }

        let results: Vec<(usize, Option<String>)> = std::thread::scope(|scope| {
            let mut chunks: Vec<Vec<(usize, ModSource, String)>> =
                (0..WORKERS).map(|_| Vec::new()).collect();
            for (n, item) in pending.into_iter().enumerate() {
                chunks[n % WORKERS].push(item);
            }
            let handles: Vec<_> = chunks
                .into_iter()
                .filter(|chunk| !chunk.is_empty())
                .map(|chunk| {
                    scope.spawn(move || {
                        chunk
                            .into_iter()
                            .map(|(index, source, id)| {
                                (index, self.translate_description(source, &id))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .flatten()
                .collect()
        });

        let mut missing = 0usize;
        for (index, translated) in results {
            match translated {
                Some(text) => hits[index].translated_description = Some(text),
                None => missing += 1,
            }
        }
        if missing == hits.len() {
            Self::push_warning(
                warnings,
                "MCIM 翻译接口暂时不可用，简介先显示原文（可在设置里把「Mod 列表注释」改回原文）",
            );
        }
    }

    fn decorate(&self, mut hit: ModHit) -> ModHit {
        self.decorate_names(&mut hit);
        hit
    }

    // -- 安装 --------------------------------------------------------------

    pub fn install(&self, req: &ModInstallRequest) -> ApiResult<kaleid_api::InstallQueued> {
        let mut operation = Operation::new(self.ctx.clone(), "mods", "mod_install");
        operation.record = operation.record.with_field("version_id", req.version_id.clone()).with_field("source", req.source.as_str()).with_field("project_id", req.project_id.clone()).with_field("file_id", req.file_id.clone()).with_field("overwrite", req.overwrite);
        operation.accepted();
        let result = self.install_traced(req, &mut operation);
        if result.is_err() { operation.finish(&result, false); }
        result
    }

    fn install_traced(&self, req: &ModInstallRequest, operation: &mut Operation) -> ApiResult<kaleid_api::InstallQueued> {
        let downloads = self.downloads.clone().ok_or_else(|| {
            ApiError::Unavailable("下载服务不可用，请先启用「版本管理」插件".into())
        })?;
        let target = self.target(&req.root_id, &req.version_id)?;
        operation.record = operation.record.clone().with_field("root_id", target.root_id.clone());
        let detail = self.detail(ModDetailQuery {
            source: req.source,
            project_id: req.project_id.clone(),
            root_id: Some(req.root_id.clone()),
            version_id: Some(req.version_id.clone()),
            force: false,
        })?;
        let file = detail
            .groups
            .iter()
            .flat_map(|g| g.files.iter())
            .find(|f| f.file_id == req.file_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("这个模组文件"))?;

        let mods_dir = PathBuf::from(&target.mods_dir);
        std::fs::create_dir_all(&mods_dir)
            .map_err(|e| ApiError::io(format!("{}: {e}", mods_dir.display())))?;
        let dest = mods_dir.join(&file.file_name);

        let policy = self.policy();
        let official_url = official_url_for(&policy, &file);
        let mirror_url = mirror_url_for_policy(&policy, &file);
        // 队列里的来源标签跟着「真正会先试的地址」走：Modrinth 官方直连显示「官方」，
        // 镜像（含所有 CurseForge 文件）显示「MCIM」。
        let source_label = if policy == MOD_SOURCE_MIRROR_FIRST || official_url.is_none() {
            "mcim"
        } else {
            "mojang"
        };

        // 目标文件已存在：内容一样就直接算装好，不一样必须用户显式同意覆盖。
        if dest.is_file() {
            let existing_sha1 = file_sha1(&dest);
            let same = match (file.sha1.as_deref(), existing_sha1.as_deref()) {
                (Some(expected), Some(actual)) => expected.eq_ignore_ascii_case(actual),
                (None, _) => {
                    file.size > 0
                        && std::fs::metadata(&dest)
                            .map(|m| m.len() == file.size)
                            .unwrap_or(false)
                }
                _ => false,
            };
            if same {
                let job = self.new_job(&target, &file, source_label);
                downloads.register(job.clone());
                job.set_status(DownloadStatus::Queued, "文件已存在，直接完成");
                job.complete();
                operation.record = operation.record.clone().with_job_id(&job.id).with_field("reason", "existing_identical").with_field("skipped", true);
                operation.finish(&Ok::<(), ApiError>(()), job.cancelled());
                return Ok(kaleid_api::InstallQueued {
                    job_id: job.id.clone(),
                });
            }
            if !req.overwrite {
                return Err(ApiError::conflict(
                    "mods 里已有同名但内容不同的文件，请勾选「允许覆盖」或先手动处理",
                ));
            }
            std::fs::remove_file(&dest)
                .map_err(|e| ApiError::io(format!("{}: {e}", dest.display())))?;
        }

        // 覆盖时顺手清掉同一模组的旧文件（只删被匹配到的）。
        if let Some(installed) = &detail.project.installed {
            let old = mods_dir.join(&installed.file_name);
            if old.is_file() && old != dest {
                let _ = std::fs::remove_file(&old);
            }
        }

        let job = self.new_job(&target, &file, source_label);
        downloads.register(job.clone());
        let item = DownloadItem {
            label: file.file_name.clone(),
            phase: "mods".into(),
            official_url,
            mirror_url,
            fallback_url: None,
            prefer_mirror: false,
            dest,
            size: file.size,
            sha1: file.sha1.clone(),
        };
        let handle = downloads.clone();
        let job_for_thread = job.clone();
        operation.record = operation.record.clone().with_job_id(&job.id).with_field("game_version", target.game_version.clone()).with_field("loader", target.loader.slug());
        let worker_operation = operation.clone();
        spawn_thread("kaleid-mod-install", move || {
                let _ = job_for_thread.start_phase("mods");
                job_for_thread.set_status(DownloadStatus::Queued, "准备下载模组");
                job_for_thread.add_total("mods", item.size, 1);
                let result = handle.run_batch(job_for_thread.clone(), vec![item]);
                match &result {
                    Ok(()) => {
                        job_for_thread.complete_phase("mods");
                        job_for_thread.complete();
                    }
                    Err(err) => job_for_thread.fail(err.to_string()),
                }
                worker_operation.finish_with_failure(&result, job_for_thread.cancelled(), job_for_thread.diagnostic_failed());
            })
            .map_err(|e| { operation.record = operation.record.clone().with_field("category", "thread_spawn"); ApiError::io(e.to_string()) })?;

        Ok(kaleid_api::InstallQueued {
            job_id: job.id.clone(),
        })
    }

    fn new_job(&self, target: &ModTarget, file: &ModFile, source: &str) -> Arc<Job> {
        let title = format!("下载模组 {}", file.file_name);
        Job::with_initial_source(
            uuid::Uuid::new_v4().to_string(),
            title,
            target.version_id.clone(),
            target.root_id.clone(),
            target.game_version.clone(),
            target.loader,
            None,
            job_policy(&self.policy()),
            source,
        )
    }
}

fn source_filter_key(filter: ModSourceFilter) -> &'static str {
    match filter {
        ModSourceFilter::All => "all",
        ModSourceFilter::Modrinth => "modrinth",
        ModSourceFilter::CurseForge => "curseforge",
    }
}

/// 当前数据源策略；老配置里的三态取值会被归一化成新取值。
///
/// 做成自由函数是因为「本地内容」插件也要按同一套策略走更新检查——两处各写一份
/// 迟早会分叉，用户切了镜像却发现更新检查还在连官方接口。
pub(crate) fn mod_policy(settings: &Settings) -> String {
    settings
        .global()
        .map(|g| normalize_mod_source(&g.mod_source))
        .ok()
        .filter(|v| is_valid_mod_source(v))
        .unwrap_or_else(|| MOD_SOURCE_OFFICIAL_FIRST.to_string())
}

/// 按策略拼出一条接口路径的候选 URL（官方 / 镜像，按尝试顺序）。
pub(crate) fn candidate_urls(policy: &str, source: ModSource, path: &str) -> Vec<String> {
    match source {
        ModSource::Modrinth => match policy {
            MOD_SOURCE_OFFICIAL_ONLY => vec![format!("{MODRINTH_OFFICIAL}{path}")],
            MOD_SOURCE_MIRROR_FIRST => vec![
                format!("{MODRINTH_MIRROR}{path}"),
                format!("{MODRINTH_OFFICIAL}{path}"),
            ],
            _ => vec![
                format!("{MODRINTH_OFFICIAL}{path}"),
                format!("{MODRINTH_MIRROR}{path}"),
            ],
        },
        ModSource::CurseForge => vec![format!("{CURSEFORGE_MIRROR}{path}")],
    }
}

/// 去重后追加一条提示：同一种失败在几十个模组上重复时，界面不该刷出几十行。
pub(crate) fn push_warning(warnings: &mut Vec<String>, message: impl Into<String>) {
    let message = message.into();
    if !warnings.iter().any(|w| *w == message) {
        warnings.push(message);
    }
}

pub(crate) fn job_policy(mod_policy: &str) -> String {
    match mod_policy {
        // 镜像优先 / 仅官方都用下载器认得的取值；「官方优先」单独加 `mod-` 前缀，
        // 因为下载器的慢速监控只认 `official-first`，模组文件不该被切成 BMCLAPI
        // （那是给游戏文件准备的镜像，模组文件没有对应物）。
        MOD_SOURCE_MIRROR_FIRST => "mod-mirror-first".to_string(),
        MOD_SOURCE_OFFICIAL_ONLY => "mod-official-only".to_string(),
        _ => "mod-official-first".to_string(),
    }
}

/// 官方模式下只给官方地址；镜像模式下只给镜像地址。
///
/// CurseForge 的文件直链在 `edge.forgecdn.net`，国内经常连不上，PCL2 也是无条件
/// 换成镜像地址——所以 CF 文件**没有**「官方直链」这一说，只有 Modrinth 有。
fn official_url_for(policy: &str, file: &ModFile) -> Option<String> {
    if file.source.as_str() != "modrinth" {
        return None;
    }
    match policy {
        // 仅官方 + 镜像优先：Modrinth 没有「只用镜像」这一档，镜像优先也保留官方兜底。
        MOD_SOURCE_OFFICIAL_ONLY | MOD_SOURCE_MIRROR_FIRST | MOD_SOURCE_OFFICIAL_FIRST => {
            Some(file.official_url.clone()).filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

fn mirror_url_for_policy(policy: &str, file: &ModFile) -> Option<String> {
    match policy {
        // 只有「Modrinth + 只要官方源」才彻底不碰镜像。
        MOD_SOURCE_OFFICIAL_ONLY if file.source.as_str() == "modrinth" => None,
        _ => file.mirror_url.clone(),
    }
}

fn version_key(family: &str) -> (u32, u32) {
    let mut parts = family.split('.');
    let major = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (major, minor)
}

fn strings_of(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Modrinth 版本对象里的主文件。
pub(crate) fn primary_file(item: &Value) -> Option<&Value> {
    let files = item.get("files")?.as_array()?;
    files
        .iter()
        .find(|f| f.get("primary").and_then(|v| v.as_bool()).unwrap_or(false))
        .or_else(|| files.first())
}

/// 沿 `inheritsFrom` 链找出这个实例对应的原版版本号。
///
/// 加载器实例自己的 id 是 `1.21.4-fabric` 这种，直接拿去查 Modrinth 一个都查不到。
/// 做成自由函数是因为更新检查也要用同一套判断：两处各写一份迟早会分叉，
/// 然后变成「热门列表里有、更新检查却查不到」这种说不清的问题。
pub(crate) fn resolve_game_version(
    versions: &crate::versions::VersionStore,
    root: &kaleid_api::Root,
    summary: &kaleid_api::VersionSummary,
) -> String {
    let mut current = summary.clone();
    let mut game = current.id.clone();
    // 链最多几层；写死 8 是防止配置写坏后死循环。
    for _ in 0..8 {
        if current.loader == LoaderKind::Vanilla {
            return current.id.clone();
        }
        let Some(parent) = current.inherits_from.clone() else {
            break;
        };
        let path = root
            .versions_dir()
            .join(&parent)
            .join(format!("{parent}.json"));
        match versions.inspect(&path) {
            Ok(next) => {
                game = next.id.clone();
                current = next;
            }
            // 父版本没装：拿父版本的名字当答案，总比拿 `1.21.4-fabric` 强。
            Err(_) => {
                game = parent;
                break;
            }
        }
    }
    game
}

/// 文件内容的 SHA-1（十六进制小写）。
///
/// 更新检查靠它认文件：Modrinth 的 `/version_files` 就吃这个值。整份读进内存是
/// 有意的——模组 jar 通常几 MB，而流式读省不下多少，却要多一层错误处理。
pub(crate) fn file_sha1(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Some(hex)
}

/// 在 `mods/` 里找这个模组：先按已知文件名，再按 slug/名称的归一化子串。
fn detect_installed(
    mods_dir: &Path,
    slug: &str,
    name: &str,
    known_files: &HashSet<String>,
) -> Option<InstalledMod> {
    let entries = std::fs::read_dir(mods_dir).ok()?;
    let mut fallback: Option<InstalledMod> = None;
    let slug_key = normalize_name(slug);
    let name_key = normalize_name(name);
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let lower = file_name.to_lowercase();
        let is_jar = lower.ends_with(".jar") || lower.ends_with(".jar.disabled");
        if !is_jar {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if known_files.contains(&lower) {
            return Some(InstalledMod {
                file_name,
                size,
                sha1: None,
                matched_by: "file_name".to_string(),
            });
        }
        if fallback.is_none() {
            let stem = normalize_name(&lower);
            let hit = (!slug_key.is_empty() && stem.contains(&slug_key))
                || (!name_key.is_empty() && stem.contains(&name_key));
            if hit {
                fallback = Some(InstalledMod {
                    file_name,
                    size,
                    sha1: None,
                    matched_by: "slug".to_string(),
                });
            }
        }
    }
    fallback
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// Modrinth 搜索响应 → 列表项。
fn parse_mr_hits(value: &Value, latest: Option<&str>) -> Vec<ModHit> {
    value
        .get("hits")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| parse_mr_hit(item, latest))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_mr_hit(item: &Value, latest: Option<&str>) -> Option<ModHit> {
    let id = item
        .get("project_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())?
        .to_string();
    let slug = item
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let name = item
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or(&slug)
        .to_string();
    let description = item
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let categories = strings_of(item.get("categories"));
    let versions = item
        .get("versions")
        .map(|v| strings_of(Some(v)))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| strings_of(item.get("game_versions")));
    let definitions = types_for_mr(&categories);
    let loaders: Vec<String> = categories
        .iter()
        .filter_map(|c| normalize_loader(c).map(|s| s.to_string()))
        .collect();
    Some(ModHit {
        source: ModSource::Modrinth,
        id,
        slug: slug.clone(),
        name,
        translated_name: None,
        description,
        translated_description: None,
        icon_url: item
            .get("icon_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        downloads: item.get("downloads").and_then(|v| v.as_u64()).unwrap_or(0),
        updated_at: item
            .get("date_modified")
            .or_else(|| item.get("updated"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        loaders_label: join_loader_labels(loaders.iter().map(|s| s.as_str())),
        loaders,
        game_range: kaleid_api::format_game_range(&versions, latest),
        type_label: primary_label(&definitions),
        tags: labels_of(&definitions),
        page_url: format!("https://modrinth.com/mod/{slug}"),
        wiki_url: None,
    })
}

/// Modrinth 项目对象（`/projects` 批量或 `/project/{id}`）→ 列表项。
fn parse_mr_project_hit(item: &Value) -> Option<ModHit> {
    parse_mr_hit(item, None)
}

fn build_project_from_modrinth(project: &Value, versions: &[String]) -> ModProject {
    let id = project
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let slug = project
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let name = project
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or(&slug)
        .to_string();
    let categories = strings_of(project.get("categories"));
    let definitions = types_for_mr(&categories);
    let loaders = strings_of(project.get("loaders"));
    ModProject {
        source: ModSource::Modrinth,
        id,
        slug: slug.clone(),
        name,
        translated_name: None,
        description: project
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        translated_description: None,
        icon_url: project
            .get("icon_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        downloads: project
            .get("downloads")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        updated_at: project
            .get("updated")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        loaders_label: join_loader_labels(loaders.iter().map(|s| s.as_str())),
        game_range: kaleid_api::format_game_range(versions, None),
        tags: labels_of(&definitions),
        page_url: format!("https://modrinth.com/mod/{slug}"),
        wiki_url: None,
        installed: None,
    }
}

fn parse_cf_hits(value: &Value, latest: Option<&str>) -> Vec<ModHit> {
    value
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| parse_cf_item_hit(item, latest))
                .collect()
        })
        .unwrap_or_default()
}

/// CurseForge 的 mod 对象（搜索项与详情项同构）→ 列表项。
fn parse_cf_item_hit(item: &Value, latest: Option<&str>) -> Option<ModHit> {
    let id = item.get("id").and_then(|v| v.as_i64())?.to_string();
    let slug = item
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&slug)
        .to_string();
    let category_ids: Vec<i64> = item
        .get("categories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("id").and_then(|v| v.as_i64()))
                .collect()
        })
        .unwrap_or_default();
    let definitions = types_for_cf(&category_ids);
    let (loaders, versions) = cf_loaders_and_versions(item);
    Some(ModHit {
        source: ModSource::CurseForge,
        id,
        slug,
        name,
        translated_name: None,
        description: item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        translated_description: None,
        icon_url: item
            .get("logo")
            .and_then(|logo| {
                logo.get("thumbnailUrl")
                    .or_else(|| logo.get("url"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        downloads: item
            .get("downloadCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        updated_at: item
            .get("dateModified")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        loaders_label: join_loader_labels(loaders.iter().map(|s| s.as_str())),
        loaders,
        game_range: kaleid_api::format_game_range(&versions, latest),
        type_label: primary_label(&definitions),
        tags: labels_of(&definitions),
        page_url: item
            .get("links")
            .and_then(|l| l.get("websiteUrl"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default(),
        wiki_url: None,
    })
}

/// CF 的 mod 对象里把加载器与 MC 版本拆出来。
fn cf_loaders_and_versions(item: &Value) -> (Vec<String>, Vec<String>) {
    let mut loaders: Vec<String> = Vec::new();
    let mut versions: Vec<String> = Vec::new();
    if let Some(indexes) = item.get("latestFilesIndexes").and_then(|v| v.as_array()) {
        for index in indexes {
            if let Some(loader) = index
                .get("modLoader")
                .and_then(|v| v.as_i64())
                .and_then(cf_loader_name)
            {
                let loader = loader.to_string();
                if !loaders.contains(&loader) {
                    loaders.push(loader);
                }
            }
            if let Some(game) = index.get("gameVersion").and_then(|v| v.as_str()) {
                if game_family(game).is_some() && !versions.contains(&game.to_string()) {
                    versions.push(game.to_string());
                }
            }
        }
    }
    if versions.is_empty() {
        if let Some(files) = item.get("latestFiles").and_then(|v| v.as_array()) {
            for file in files {
                for game in strings_of(file.get("gameVersions")) {
                    if let Some(loader) = normalize_loader(&game) {
                        let loader = loader.to_string();
                        if !loaders.contains(&loader) {
                            loaders.push(loader);
                        }
                    } else if game_family(&game).is_some() && !versions.contains(&game) {
                        versions.push(game);
                    }
                }
            }
        }
    }
    (loaders, versions)
}

fn cf_loader_name(id: i64) -> Option<&'static str> {
    match id {
        1 => Some("forge"),
        4 => Some("fabric"),
        5 => Some("quilt"),
        6 => Some("neoforge"),
        _ => None,
    }
}

fn build_project_from_curseforge(item: &Value, versions: &[String]) -> ModProject {
    let id = item
        .get("id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .to_string();
    let slug = item
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&slug)
        .to_string();
    let category_ids: Vec<i64> = item
        .get("categories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("id").and_then(|v| v.as_i64()))
                .collect()
        })
        .unwrap_or_default();
    let definitions = types_for_cf(&category_ids);
    let (loaders, _) = cf_loaders_and_versions(item);
    ModProject {
        source: ModSource::CurseForge,
        id,
        slug,
        name,
        translated_name: None,
        description: item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        translated_description: None,
        icon_url: item
            .get("logo")
            .and_then(|logo| {
                logo.get("thumbnailUrl")
                    .or_else(|| logo.get("url"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        downloads: item
            .get("downloadCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        updated_at: item
            .get("dateModified")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        loaders_label: join_loader_labels(loaders.iter().map(|s| s.as_str())),
        game_range: kaleid_api::format_game_range(versions, None),
        tags: labels_of(&definitions),
        page_url: item
            .get("links")
            .and_then(|l| l.get("websiteUrl"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default(),
        wiki_url: None,
        installed: None,
    }
}

/// URL 参数百分号编码（只处理查询串里会出现的字符）。
pub(crate) fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 「热门」分面与中文关键词（纯函数：不联网、好单测）
// ---------------------------------------------------------------------------

/// 模组类型 id → Modrinth 的分类 slug。
///
/// 类型表里有一半条目只存在于 CurseForge（`cf407` 这种），把它们塞进 Modrinth
/// 的 facets 只会得到零结果，所以查不到 slug 时宁可不加这一组。
fn modrinth_slug_of(type_id: &str) -> Option<&'static str> {
    type_by_id(type_id).and_then(|def| def.mr.first().copied())
}

/// PCL2 式的「热门」分面串：
/// `[["project_type:mod"],["categories:<类型>"],["versions:<游戏版本>"],["categories:<加载器>"]]`。
///
/// 分组顺序是固定的：Modrinth 的 facets 是「组间 AND、组内 OR」，先写项目类型
/// 再写分类/版本/加载器，出问题时从 URL 上一眼就能看出是哪一层筛掉的。
///
/// 空条件**整组省略**：`["versions:"]` 这种空值会被 Modrinth 当成非法 facets
/// 直接 400，而不是"不过滤"。
pub(crate) fn ranking_facets(game: &str, loader: &str, type_id: Option<&str>) -> String {
    let mut groups: Vec<String> = vec![r#"["project_type:mod"]"#.to_string()];
    let type_id = type_id.map(str::trim).filter(|id| !id.is_empty());
    if let Some(slug) = type_id.and_then(modrinth_slug_of) {
        groups.push(format!(r#"["categories:{slug}"]"#));
    }
    // 游戏版本这一组必须先过一遍"像不像版本号"：版本文件夹可以叫任何名字
    // （整合包名、`1.21.4-fabric` 之类），拿去当 `versions:` 分面查必然 0 命中，
    // 于是初始排行整页变空、还告诉用户"这个筛选条件下没有模组"。
    let game = game.trim();
    if looks_like_mc_version(game) {
        groups.push(format!(r#"["versions:{game}"]"#));
    }
    if let Some(loader) = normalize_loader(loader) {
        groups.push(format!(r#"["categories:{loader}"]"#));
    }
    format!("[{}]", groups.join(","))
}

/// 这段文本像不像一个真正的 Minecraft 原版版本号？
///
/// 只用于**分面**这一条路径：版本目录的名字由用户/整合包决定，`mods.target()`
/// 在"原版"分支会直接把它当成游戏版本返回（见 [`resolve_game_version`]）。
/// 于是 `The Last Voyage of the Harpy Express` 这种名字会被当成 `versions:` 分面，
/// Modrinth 自然一条也查不到——认不出是版本号时宁可不加这一组。
///
/// 认的形式：正式版 `1.21.4` / 两段式 `26.3` / 预发布 `1.20.5-pre1`、`-rc1` /
/// 快照 `24w14a`、`23w13a_or_b`。
pub(crate) fn looks_like_mc_version(text: &str) -> bool {
    let text = text.trim();
    if text.len() < 3 || text.len() > 24 {
        return false;
    }
    // 正式版 / 预发布：`1.21.4`、`26.3`、`1.20.5-pre2`、`1.20.5-rc1`
    let (head, tail) = match text.split_once('-') {
        Some((head, tail)) => (head, Some(tail)),
        None => (text, None),
    };
    let segs: Vec<&str> = head.split('.').collect();
    let head_ok = (2..=3).contains(&segs.len())
        && segs
            .iter()
            .all(|seg| !seg.is_empty() && seg.len() <= 3 && seg.bytes().all(|b| b.is_ascii_digit()));
    if head_ok {
        let Some(tail) = tail else { return true };
        return ["pre", "rc"].iter().any(|prefix| {
            tail.strip_prefix(prefix)
                .map(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .unwrap_or(false)
        });
    }
    // 快照：`24w14a`、`23w13a_or_b`、`20w14infinite`
    let bytes = text.as_bytes();
    if bytes.len() >= 6
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'w'
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit()
        && bytes[5].is_ascii_lowercase()
    {
        // 前 6 个字节全是 ASCII，切片边界安全。
        return text[6..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    }
    false
}

/// 排行 0 命中时该说哪句话。
///
/// 没有任何筛选条件时，"这个筛选条件下没有模组"是在误导用户（初始列表本来就
/// 应该是下载量排行，空只可能是接口/镜像抽风）。
pub(crate) fn ranking_empty_warning(has_filter: bool) -> &'static str {
    if has_filter {
        "这个筛选条件下 Modrinth 没有可用的模组"
    } else {
        "Modrinth 没有返回任何模组，稍后再试或点「刷新」"
    }
}

/// 「热门」缓存键：同一组筛选参数必须落在同一个键上。
///
/// 不直接拿 facets 当键：facets 里没有「注释档位」和页码，切到译文或者翻页时
/// 会互相覆盖，用户会看到一页混合了原文/译文的结果。
pub(crate) fn ranking_cache_key(
    game: &str,
    loader: &str,
    type_id: Option<&str>,
    description_mode: &str,
    page: u32,
) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        game.trim(),
        normalize_loader(loader).unwrap_or(""),
        type_id.map(str::trim).unwrap_or(""),
        description_mode,
        page
    )
}

/// 查询里是否含汉字（顺带认日文假名与谚文）。
///
/// 只对这类查询做「中文名 → slug」的翻译：纯英文/数字的查询本来就搜得到，
/// 多查一次词表反而可能把 `create` 这种既是模组名又是动词的词翻歪。
pub(crate) fn is_zh(query: &str) -> bool {
    query.chars().any(|ch| {
        matches!(
            u32::from(ch),
            // 日文假名
            0x3040..=0x30FF
            // 汉字扩展 A
            | 0x3400..=0x4DBF
            // 常用汉字
            | 0x4E00..=0x9FFF
            // 谚文
            | 0xAC00..=0xD7AF
            // 兼容汉字
            | 0xF900..=0xFAFF
        )
    })
}

/// 中文关键词 → 站点能搜的词：优先 slug（Modrinth 的搜索对 slug 最准），
/// 其次英文名。
///
/// 词表里没有就返回 `None`，由调用方退回原始 query——这是"翻不出来"而不是
/// "出错"，报错会让用户以为搜索坏了。
pub(crate) fn translate_query(names: &ModNames, query: &str) -> Option<String> {
    let entry = names.lookup_zh(query)?;
    let slug = entry
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|slug| !slug.is_empty());
    let english = Some(entry.en.trim()).filter(|name| !name.is_empty());
    slug.map(str::to_string)
        .or_else(|| english.map(str::to_string))
}

// ---------------------------------------------------------------------------
// 查询参数
// ---------------------------------------------------------------------------

/// 一次「热门」查询：PCL2 的做法是按当前版本 + 加载器 + 类型分面，取一页 50 个。
///
/// 三个筛选条件都允许缺席：用户还没选版本、或者选的是原版时，不该拦着看热门，
/// 只是分面里少一组（见 [`ranking_facets`]）。
#[derive(Debug, Clone, Default)]
pub struct ModRankingQuery {
    /// 原版版本号（`1.21.4`）。
    pub game_version: Option<String>,
    /// `fabric` / `forge` / `neoforge`；原版留空。
    pub loader: Option<String>,
    /// 类型表里的 id（见 [`mod_types`]）。
    pub type_id: Option<String>,
    /// 0 起的页码，一页固定 [`RANKING_PAGE_SIZE`] 条。
    pub page: Option<u32>,
    /// 跳过缓存重新抓。
    pub force: bool,
}

/// 安装一个具体的模组文件。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModInstallRequest {
    pub root_id: String,
    pub version_id: String,
    pub source: ModSource,
    pub project_id: String,
    pub file_id: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("mods_diagnostics_tests.rs");

    #[test]
    fn diagnostics_mod_preflight_is_one_safe_operation_result() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = kaleid_kernel::Kernel::new();
        let records = Arc::new(Mutex::new(Vec::<kaleid_kernel::DiagnosticRecord>::new()));
        let captured = records.clone();
        kernel.context().on::<kaleid_kernel::DiagnosticRecord>(move |r| captured.lock().unwrap().push(r.clone()));
        let settings = Arc::new(Settings::open(dir.path()));
        let roots = Arc::new(RootRegistry::empty_at(dir.path()).unwrap());
        let service = ModService::new(settings, roots, Arc::new(VersionStore::new()), None, dir.path()).with_context(kernel.context().clone());
        let req = ModInstallRequest { root_id: "root".into(), version_id: "fixture".into(), source: ModSource::Modrinth, project_id: "project".into(), file_id: "file".into(), overwrite: false };
        assert!(service.install(&req).is_err());
        let records = records.lock().unwrap();
        let ended: Vec<_> = records.iter().filter(|r| r.event == "operation.finished").collect();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].fields["operation"], "mod_install");
        assert_eq!(ended[0].fields["category"], "unavailable");
        assert_eq!(ended[0].fields["project_id"], "project");
        assert_eq!(ended[0].fields["file_id"], "file");
        assert!(!ended[0].fields.contains_key("root_id"), "unavailable downloader prevents root validation");
        assert!(records.iter().filter(|r| r.event == "operation.accepted").all(|r| !r.fields.contains_key("root_id")));
    }


    #[test]
    fn user_agent_matches_mcim_whitelist_shape() {
        assert!(USER_AGENT.starts_with("KMLH/"), "UA 必须是 KMLH/<version>");
        let code = USER_AGENT.trim_start_matches("KMLH/");
        assert!(!code.is_empty());
        assert!(code
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())));
    }

    #[test]
    fn type_table_is_consistent() {
        let mut ids = HashSet::new();
        for def in TYPES {
            assert!(ids.insert(def.id), "类型 id 重复：{}", def.id);
            assert!(
                def.cf.is_some() || !def.mr.is_empty(),
                "类型 {} 至少要有一个源支持",
                def.id
            );
            assert!(!def.group.is_empty());
        }
        assert_eq!(type_by_id("worldgen").unwrap().cf, Some(406));
        assert_eq!(mod_types().len(), TYPES.len());
    }

    #[test]
    fn cf_category_labels_come_from_the_table() {
        let defs = types_for_cf(&[6814, 424]);
        let labels = labels_of(&defs);
        assert!(labels.contains(&"性能优化".to_string()));
        assert!(labels.contains(&"装饰".to_string()));
        let defs = types_for_mr(&["worldgen".to_string()]);
        assert_eq!(primary_label(&defs), "世界生成");
        assert_eq!(primary_label(&[]), "其他");
    }

    #[test]
    fn modrinth_search_hits_are_parsed() {
        let value = json!({
            "hits": [{
                "project_id": "AANobbMI",
                "slug": "sodium",
                "title": "Sodium",
                "description": "A high-performance rendering engine replacement",
                "categories": ["optimization", "fabric", "neoforge"],
                "versions": ["1.16.5", "1.20.1", "1.21.4"],
                "downloads": 257195667,
                "icon_url": "https://cdn.modrinth.com/data/AANobbMI/icon.png",
                "date_modified": "2026-10-16T10:00:00Z"
            }]
        });
        let hits = parse_mr_hits(&value, Some("1.21.4"));
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.name, "Sodium");
        assert_eq!(hit.source, ModSource::Modrinth);
        assert_eq!(hit.downloads, 257_195_667);
        assert_eq!(hit.loaders_label, "Fabric / NeoForge");
        assert_eq!(hit.game_range, "1.16, 1.20+");
        assert_eq!(hit.type_label, "性能优化");
        assert_eq!(hit.page_url, "https://modrinth.com/mod/sodium");
        assert_eq!(
            hit.icon_url.as_deref(),
            Some("https://cdn.modrinth.com/data/AANobbMI/icon.png")
        );
    }

    #[test]
    fn curseforge_search_hits_are_parsed() {
        let value = json!({
            "data": [{
                "id": 238222,
                "slug": "jei",
                "name": "Just Enough Items",
                "summary": "View Items and Recipes",
                "downloadCount": 321456789,
                "dateModified": "2026-10-01T08:00:00Z",
                "logo": { "thumbnailUrl": "https://media.forgecdn.net/thumb.jpg", "url": "https://media.forgecdn.net/logo.png" },
                "links": { "websiteUrl": "https://www.curseforge.com/minecraft/mc-mods/jei" },
                "categories": [{ "id": 423, "name": "Map and Information" }],
                "latestFilesIndexes": [
                    { "gameVersion": "1.21.1", "modLoader": 6, "fileId": 1, "filename": "jei-1.21.1.jar" },
                    { "gameVersion": "1.20.1", "modLoader": 1, "fileId": 2, "filename": "jei-1.20.1.jar" }
                ]
            }],
            "pagination": { "totalCount": 42 }
        });
        let hits = parse_cf_hits(&value, None);
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.id, "238222");
        assert_eq!(
            hit.icon_url.as_deref(),
            Some("https://media.forgecdn.net/thumb.jpg")
        );
        assert_eq!(hit.loaders_label, "Forge / NeoForge");
        assert_eq!(hit.game_range, "1.20-1.21");
        assert_eq!(hit.type_label, "信息显示");
        assert_eq!(
            hit.page_url,
            "https://www.curseforge.com/minecraft/mc-mods/jei"
        );
    }

    #[test]
    fn ranking_facets_follow_pcl2_group_order() {
        // 三组都在：项目类型 → 分类 → 游戏版本 → 加载器，顺序固定
        assert_eq!(
            ranking_facets("1.21.4", "Fabric", Some("optimization")),
            r#"[["project_type:mod"],["categories:optimization"],["versions:1.21.4"],["categories:fabric"]]"#
        );
        // 没有类型时**不能**多出一组空数组
        assert_eq!(
            ranking_facets("1.21.4", "fabric", None),
            r#"[["project_type:mod"],["versions:1.21.4"],["categories:fabric"]]"#
        );
        // 只存在于 CurseForge 的类型（没有 Modrinth slug）同样不加这一组
        assert_eq!(
            ranking_facets("1.21.4", "fabric", Some("cf407")),
            r#"[["project_type:mod"],["versions:1.21.4"],["categories:fabric"]]"#
        );
        // 原版 / 未识别加载器 + 未知游戏版本：只剩项目类型，绝不发 `["versions:"]`
        assert_eq!(ranking_facets("", "vanilla", None), r#"[["project_type:mod"]]"#);
        assert_eq!(ranking_facets("  ", "", Some("  ")), r#"[["project_type:mod"]]"#);
        // 加载器写法不统一（`NeoForge` / `neoforge`）要归一成同一条 facet
        assert_eq!(
            ranking_facets("1.21.1", "NeoForge", None),
            ranking_facets("1.21.1", "neoforge", None)
        );
    }

    #[test]
    fn ranking_facets_drop_names_that_are_not_game_versions() {
        // 版本文件夹名完全可能是"整合包名字"：`The Last Voyage of the Harpy Express`
        // 这种当 `versions:` 分面去查必然 0 命中，于是整页排行变成"没有可用的模组"。
        // 认不出是原版版本号时就不该生成这一组。
        assert_eq!(
            ranking_facets("The Last Voyage of the Harpy Express", "fabric", None),
            r#"[["project_type:mod"],["categories:fabric"]]"#
        );
        assert_eq!(
            ranking_facets("1.21.4-fabric", "", None),
            r#"[["project_type:mod"]]"#
        );
        // 真版本号仍然要进分面
        assert_eq!(
            ranking_facets("1.21.4", "", None),
            r#"[["project_type:mod"],["versions:1.21.4"]]"#
        );
    }

    #[test]
    fn looks_like_mc_version_accepts_release_and_snapshot_names() {
        for ok in ["1.21.4", "1.7.10", "26.3", "1.21", "24w14a", "1.20.5-pre1"] {
            assert!(looks_like_mc_version(ok), "{ok} 应该被认成版本号");
        }
        for bad in [
            "",
            "  ",
            "fabric-loader-0.16.9-1.21.4",
            "The Last Voyage of the Harpy Express",
            "整合包",
            "1.21.4-forge",
        ] {
            assert!(!looks_like_mc_version(bad), "{bad} 不该被认成版本号");
        }
    }

    #[test]
    fn empty_ranking_warning_depends_on_whether_anything_was_filtered() {
        // 没有任何筛选条件时再说"这个筛选条件下"是在骗人：初始列表就该是
        // 下载量排行，0 命中只可能是接口/镜像的问题。
        assert_eq!(
            ranking_empty_warning(false),
            "Modrinth 没有返回任何模组，稍后再试或点「刷新」"
        );
        assert_eq!(
            ranking_empty_warning(true),
            "这个筛选条件下 Modrinth 没有可用的模组"
        );
    }

    #[test]
    fn ranking_cache_key_separates_every_filter() {
        let base = ranking_cache_key("1.21.4", "fabric", None, "original", 0);
        assert_eq!(
            base,
            ranking_cache_key(" 1.21.4 ", "Fabric", None, "original", 0),
            "同一组参数（大小写/空格不同）必须命中同一份缓存"
        );
        assert_ne!(
            base,
            ranking_cache_key("1.21.4", "fabric", Some("optimization"), "original", 0)
        );
        assert_ne!(
            base,
            ranking_cache_key("1.20.1", "fabric", None, "original", 0)
        );
        assert_ne!(
            base,
            ranking_cache_key("1.21.4", "forge", None, "original", 0)
        );
        // 注释档位不同的结果不能共用：一份有译文、一份没有
        assert_ne!(
            base,
            ranking_cache_key("1.21.4", "fabric", None, "translated", 0)
        );
        // 翻页不能覆盖第一页
        assert_ne!(
            base,
            ranking_cache_key("1.21.4", "fabric", None, "original", 1)
        );
    }

    #[test]
    fn chinese_queries_are_detected() {
        assert!(is_zh("机械动力"));
        assert!(is_zh("钠"));
        assert!(is_zh("Create 机械动力"));
        assert!(is_zh("サーバー"));
        // 纯英文 / 数字 / 空格不算：这类查询本来就能直接搜到
        assert!(!is_zh("sodium"));
        assert!(!is_zh("Create"));
        assert!(!is_zh("journeymap 5.9"));
        assert!(!is_zh(""));
    }

    #[test]
    fn chinese_query_translation_prefers_slug_then_falls_back() {
        let names = ModNames::from_json(
            r#"{"entries":[
                {"zh":"机械动力","en":"Create","slug":"create"},
                {"zh":"苹果皮","en":"AppleSkin"}
            ]}"#,
        );
        assert_eq!(
            translate_query(&names, "机械动力").as_deref(),
            Some("create")
        );
        // 没有 slug 时用英文名，否则站点搜不到
        assert_eq!(
            translate_query(&names, "苹果皮").as_deref(),
            Some("AppleSkin")
        );
        // 词表里没有 → None，调用方原样使用原始 query（不是报错）
        assert_eq!(translate_query(&names, "不存在的模组"), None);
    }

    #[test]
    fn percent_encoding_handles_facets_and_quotes() {
        assert_eq!(
            percent_encode(r#"[["project_type:mod"]]"#),
            "%5B%5B%22project_type%3Amod%22%5D%5D"
        );
        assert_eq!(percent_encode("create mod"), "create%20mod");
    }

    #[test]
    fn download_policy_maps_to_job_policy() {
        assert_eq!(job_policy(MOD_SOURCE_MIRROR_FIRST), "mod-mirror-first");
        assert_eq!(job_policy(MOD_SOURCE_OFFICIAL_ONLY), "mod-official-only");
        assert_eq!(job_policy(MOD_SOURCE_OFFICIAL_FIRST), "mod-official-first");
    }

    #[test]
    fn only_modrinth_files_have_an_official_direct_link() {
        let mut file = ModFile {
            source: ModSource::Modrinth,
            project_id: "AANobbMI".into(),
            file_id: "RncWhTxD".into(),
            name: "Sodium 0.5.11".into(),
            file_name: "sodium.jar".into(),
            version_number: "0.5.11".into(),
            size: 1,
            downloads: 0,
            published_at: "2026-01-01T00:00:00Z".into(),
            release_type: "release".into(),
            loaders: vec!["fabric".into()],
            game_versions: vec!["1.21".into()],
            official_url: "https://cdn.modrinth.com/data/AANobbMI/x.jar".into(),
            mirror_url: Some("https://mod.mcimirror.top/data/AANobbMI/x.jar".into()),
            sha1: None,
            required_deps: 0,
        };
        // Modrinth：三种策略都给官方直链（镜像优先也有官方兜底），镜像只在非「仅官方」时挂上。
        assert!(official_url_for(MOD_SOURCE_OFFICIAL_FIRST, &file).is_some());
        assert!(mirror_url_for_policy(MOD_SOURCE_OFFICIAL_FIRST, &file).is_some());
        assert!(official_url_for(MOD_SOURCE_MIRROR_FIRST, &file).is_some());
        assert!(mirror_url_for_policy(MOD_SOURCE_MIRROR_FIRST, &file).is_some());
        assert!(mirror_url_for_policy(MOD_SOURCE_OFFICIAL_ONLY, &file).is_none());

        // CurseForge：永远只有镜像地址（edge.forgecdn.net 国内连不上）。
        file.source = ModSource::CurseForge;
        file.official_url = "https://edge.forgecdn.net/files/1/2/x.jar".into();
        for policy in [
            MOD_SOURCE_OFFICIAL_FIRST,
            MOD_SOURCE_MIRROR_FIRST,
            MOD_SOURCE_OFFICIAL_ONLY,
        ] {
            assert!(
                official_url_for(policy, &file).is_none(),
                "「{policy}」不该给 CF 官方直链"
            );
            assert!(
                mirror_url_for_policy(policy, &file).is_some(),
                "「{policy}」必须留下镜像地址"
            );
        }
    }

    #[test]
    fn installed_detection_prefers_exact_file_then_slug() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sodium-fabric-0.6.0.jar"), b"a").unwrap();
        std::fs::write(dir.path().join("other.jar"), b"b").unwrap();
        let known: HashSet<String> = ["sodium-fabric-0.6.0.jar".to_string()].into();
        let hit = detect_installed(dir.path(), "sodium", "Sodium", &known).unwrap();
        assert_eq!(hit.matched_by, "file_name");

        // 已知文件名不匹配时，用 slug 里的词做兜底
        let empty: HashSet<String> = HashSet::new();
        let hit = detect_installed(dir.path(), "sodium", "Sodium", &empty).unwrap();
        assert_eq!(hit.matched_by, "slug");

        // 完全无关的模组不应误报
        assert!(detect_installed(dir.path(), "mekanism", "Mekanism", &empty).is_none());
        // 目录不存在也不应 panic
        assert!(detect_installed(&dir.path().join("nope"), "sodium", "Sodium", &empty).is_none());
    }
}
