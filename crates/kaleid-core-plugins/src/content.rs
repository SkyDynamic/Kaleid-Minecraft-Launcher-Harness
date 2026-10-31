//! 本地游戏内容：模组 / 资源包 / 存档 / 日志（`ContentServiceApi`）。
//!
//! # 位置规则
//!
//! 四类内容**全部跟随版本隔离**：`<根>` 还是 `<根>/versions/<id>` 由
//! [`SettingsServiceApi::resolve`] 的结论决定，和启动计划里的 `--gameDir`
//! 是同一个目录（[`resolve_game_dir`] 是唯一实现，模组下载也走它）。
//! 位置一旦分叉，界面里看到的模组就不是游戏真正加载的模组。
//!
//! # 边界
//!
//! - 目录不存在 → 空列表，**不创建空目录**（读操作不写盘）。
//! - 文件名参数只接受"直接子项"，`..`、分隔符、盘符一律拒绝。
//! - 资源包启用态**只读**：来自 `options.txt`，本服务从不写回。
//! - 导入不覆盖同名文件；删除失败原样上抛（多半是游戏正开着占了文件）。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use serde_json::{json, Value};

use kaleid_api::{
    mirror_url_for, normalize_loader, ApiError, ApiResult, ContentDirs, ContentKind,
    ContentServiceApi, DownloadStatus, ImportOutcome, ImportSkipped, LoaderKind, LocalMod, LocalPack,
    LocalSave, LogFile, LogTail, ModIcon, ModSource, ModUpdate, ModUpdateReport, Root,
    RootRegistryService, SettingsServiceApi, VersionStoreService,
};

use crate::downloads::{DownloadItem, DownloadManager, Job};
use crate::jar::{self, ModMeta};
use crate::modnames::ModNames;
use crate::mods;
use crate::nbt;
use crate::roots::RootRegistry;
use crate::settings::Settings;
use crate::versions::VersionStore;

/// 日志预览默认读多少字节。
pub const LOG_TAIL_DEFAULT: usize = 512 * 1024;
/// 日志预览最多读多少字节：再多界面也渲染不动。
pub const LOG_TAIL_MAX: usize = 2 * 1024 * 1024;
/// `.log.gz` 解压上限（防压缩炸弹）。
const LOG_INFLATE_MAX: u64 = 16 * 1024 * 1024;
/// 单个存档算体积时最多看这么多文件。
const SAVE_WALK_LIMIT: usize = 5000;
/// 存档体积遍历的最大深度（`region/` 只有两层，留足余量）。
const SAVE_WALK_DEPTH: usize = 8;
/// 图标条目的读取上限：常规模组图标是 32~256 px 的 PNG，512 KiB 已很宽容。
/// 超了就当"没有图标"——把几 MB 的图塞进 base64 会让界面卡在解码上。
const MAX_ICON_BYTES: usize = 512 * 1024;
/// 更新检查最多同时开几条连接。
///
/// 不跟着下载并发走：下载是用户主动点的大任务，更新检查是"顺手扫一眼"，
/// 一气开几十条连接既容易触发限流，也会把面板拖成转圈。
const UPDATE_MAX_WORKERS: usize = 4;
/// 整次更新检查的总时限：到点就带着已有结果返回，不让界面一直等。
const UPDATE_DEADLINE: Duration = Duration::from_secs(20);
/// 单条元数据请求的读超时（比总时限短，保证一条慢请求吃不掉整个预算）。
const UPDATE_READ_TIMEOUT: Duration = Duration::from_secs(8);
/// 更新备份目录名：旧文件搬到这里而不是直接删，用户还能自己回滚。
const BACKUP_DIR: &str = ".kaleid-backup";

/// 解析某个版本的生效游戏目录。
///
/// 返回 `(根, gameDir, 是否隔离, 结论来源)`。这是全仓唯一一处"隔离 → 目录"的
/// 换算：模组下载、内容列举、日志读取都用它，避免各算一套后互相打架。
pub fn resolve_game_dir(
    roots: &dyn RootRegistryService,
    settings: &dyn SettingsServiceApi,
    root_id: &str,
    version_id: &str,
) -> ApiResult<(Root, PathBuf, bool, String)> {
    let root = roots.get(root_id)?;
    let effective = settings.resolve(root_id, version_id, root.isolation)?;
    let game_dir = root.game_dir(version_id, effective.isolated);
    Ok((root, game_dir, effective.isolated, effective.isolated_from))
}

/// 本地内容服务。
pub struct ContentService {
    settings: Arc<Settings>,
    roots: Arc<RootRegistry>,
    names: Arc<ModNames>,
    /// jar 解析结果缓存：开一次版本页不该把几百个 jar 重拆一遍。
    mod_meta: ModMetaCache,
    /// `version-manager` 停用时为 `None`：列表、图标、更新检查都还能用，只是装不了更新。
    downloads: Option<Arc<DownloadManager>>,
    /// 版本扫描服务：更新检查要知道"这是哪个原版版本 + 哪个加载器"。
    versions: Option<Arc<VersionStore>>,
}

impl ContentService {
    pub fn new(settings: Arc<Settings>, roots: Arc<RootRegistry>, names: Arc<ModNames>) -> Self {
        ContentService {
            settings,
            roots,
            names,
            mod_meta: ModMetaCache::default(),
            downloads: None,
            versions: None,
        }
    }

    /// 挂上下载器（由 `version-manager` 提供）。没有它也能用，只是 `apply_update` 会报"缺少下载服务"。
    pub fn with_downloads(mut self, downloads: Option<Arc<DownloadManager>>) -> Self {
        self.downloads = downloads;
        self
    }

    /// 挂上版本扫描服务（更新检查要按游戏版本 + 加载器筛文件）。
    pub fn with_versions(mut self, versions: Option<Arc<VersionStore>>) -> Self {
        self.versions = versions;
        self
    }

    fn game_dir(&self, root_id: &str, version_id: &str) -> ApiResult<(PathBuf, bool, String)> {
        let (_, dir, isolated, from) = resolve_game_dir(
            self.roots.as_ref(),
            self.settings.as_ref(),
            root_id,
            version_id,
        )?;
        Ok((dir, isolated, from))
    }

    /// 某一类内容的落地目录。
    fn area_dir(&self, root_id: &str, version_id: &str, kind: ContentKind) -> ApiResult<PathBuf> {
        let (game_dir, _, _) = self.game_dir(root_id, version_id)?;
        Ok(game_dir.join(match kind {
            ContentKind::Mod => "mods",
            ContentKind::ResourcePack => "resourcepacks",
        }))
    }
}

// ---------------------------------------------------------------------------
// 通用小工具
// ---------------------------------------------------------------------------

fn modified_secs(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// 只接受"目录里的一个直接子项名"。
///
/// 这是内容命令的安全边界：界面传来的是字符串，一旦允许 `../` 或绝对路径，
/// `delete` 就成了任意路径删除器。
fn plain_name(name: &str) -> ApiResult<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::invalid("文件名不能为空"));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(ApiError::invalid("非法的文件名"));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains(':') {
        return Err(ApiError::invalid(format!("非法的文件名：{name}")));
    }
    Ok(trimmed)
}

fn read_dir_sorted(dir: &Path) -> ApiResult<Vec<(String, fs::DirEntry)>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(dir).map_err(|e| ApiError::io(format!("{}: {e}", dir.display())))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        out.push((entry.file_name().to_string_lossy().to_string(), entry));
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    Ok(out)
}

/// 从文件名猜出展示名与版本号。
///
/// `sodium-fabric-0.6.9.jar` → `("sodium-fabric", Some("0.6.9"))`。
/// 猜不准是常态，所以界面同时保留原始文件名一列。
fn split_name_version(stem: &str) -> (String, Option<String>) {
    let mut parts: Vec<&str> = stem.split('-').collect();
    let mut version: Vec<&str> = Vec::new();
    while let Some(last) = parts.last().copied() {
        if !looks_like_version(last) {
            break;
        }
        version.insert(0, parts.pop().unwrap_or_default());
    }
    // 整个名字就是一个版本号（`1.2.3.jar`）：没有可拆的东西
    if parts.is_empty() {
        return (stem.to_string(), None);
    }
    let name = parts.join("-");
    let version = if version.is_empty() {
        None
    } else {
        Some(version.join("-"))
    };
    (name, version)
}

fn looks_like_version(seg: &str) -> bool {
    let s = seg.trim_start_matches(|c| c == 'v' || c == 'V');
    if s.is_empty() || !s.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '+' || c == '_' || c == '-')
}

// ---------------------------------------------------------------------------
// 模组
// ---------------------------------------------------------------------------

/// jar 元数据缓存里的一条。
struct ModMetaEntry {
    /// 文件修改时间（epoch 秒）与大小：任一变化就当作"换过文件"，重新解析。
    mtime: u64,
    size: u64,
    /// 解析结果。`None` = 读不出模组元数据（坏 jar / 不是模组），也是要缓存的事实，
    /// 否则每次开页面都会再拆一次这个坏文件。
    meta: Option<ModMeta>,
}

/// jar 解析结果缓存。
///
/// 为什么要缓存：打开版本页就要列一遍 `mods/`，整合包动辄两三百个 jar，每个都
/// 读中央目录 + 解压元数据要好几百毫秒到几秒。jar 不会自己改内容，用「目录 +
/// 文件名 + mtime + size」做失效条件足够可靠（复制/覆盖/改名都会改这三项）。
#[derive(Default)]
struct ModMetaCache {
    entries: std::sync::Mutex<HashMap<(String, String), ModMetaEntry>>,
}

impl ModMetaCache {
    /// 取某个文件的模组元数据；读不出来返回 `None`（调用方回落到文件名启发式）。
    fn load(&self, dir: &Path, file_name: &str, path: &Path, mtime: u64, size: u64) -> Option<ModMeta> {
        let key = (dir.to_string_lossy().to_string(), file_name.to_string());
        {
            let map = self.entries.lock().unwrap();
            if let Some(entry) = map.get(&key) {
                if entry.mtime == mtime && entry.size == size {
                    return entry.meta.clone();
                }
            }
        }
        // 解析要拆 zip：**不持锁做 IO**，否则图标请求会和列表请求互相排队。
        let meta = jar::read_mod_meta(path);
        self.entries.lock().unwrap().insert(
            key,
            ModMetaEntry {
                mtime,
                size,
                meta: meta.clone(),
            },
        );
        meta
    }

    /// 目录里已经不存在的文件不再占内存（更新/删除之后会留下孤儿条目）。
    fn prune(&self, dir: &Path, keep: &HashSet<String>) {
        let prefix = dir.to_string_lossy().to_string();
        self.entries
            .lock()
            .unwrap()
            .retain(|(cached_dir, name), _| cached_dir != &prefix || keep.contains(name));
    }
}

/// 把 `mods/` 里的一批文件读成 [`LocalMod`]。
///
/// 名字优先级（与契约文档一致）：jar 内真实名 > 名称表中文名 > 文件名；
/// 版本号同理优先用 jar 里声明的，读不到才回落文件名里的数字。
fn read_mods(dir: &Path, names: &ModNames, cache: &ModMetaCache) -> ApiResult<Vec<LocalMod>> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut scanned = 0usize;
    for (file_name, entry) in read_dir_sorted(dir)? {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        scanned += 1;
        let (stem, enabled) = if let Some(base) = file_name.strip_suffix(".jar.disabled") {
            (base, false)
        } else if let Some(base) = file_name.strip_suffix(".jar") {
            (base, true)
        } else {
            continue;
        };
        seen.insert(file_name.clone());
        let (guessed_name, guessed_version) = split_name_version(stem);
        // 坏 jar、非模组 jar 都走这里：不是错误，只是没有元数据可用。
        let jar_meta = cache.load(
            dir,
            &file_name,
            &entry.path(),
            modified_secs(&meta).unwrap_or(0),
            meta.len(),
        );

        let mod_id = jar_meta.as_ref().and_then(|m| m.mod_id.clone());
        let real_name = jar_meta.as_ref().and_then(|m| m.name.clone());
        // 中文名查表的两个键：jar 里的英文名配 mod id（多半就是站点 slug），
        // 没有元数据时退回文件名拆出来的名字。
        let zh = names
            .translated(real_name.as_deref().unwrap_or(&guessed_name), mod_id.as_deref())
            .or_else(|| names.translated(&guessed_name, None));
        let display_name = real_name
            .clone()
            .or(zh)
            .unwrap_or_else(|| guessed_name.clone());
        let version = jar_meta
            .as_ref()
            .and_then(|m| m.version.clone())
            .or(guessed_version);

        out.push(LocalMod {
            display_name,
            version,
            file_name,
            size_bytes: meta.len(),
            enabled,
            modified_at: modified_secs(&meta),
            mod_id,
            name: real_name,
            authors: jar_meta.as_ref().map(|m| m.authors.clone()).unwrap_or_default(),
            flavors: jar_meta.as_ref().map(|m| m.flavors.clone()).unwrap_or_default(),
            has_icon: jar_meta
                .as_ref()
                .map(|m| m.icon_entry.is_some())
                .unwrap_or(false),
            meta_source: Some(
                jar_meta
                    .as_ref()
                    .map(|m| m.source.clone())
                    .unwrap_or_else(|| "filename".to_string()),
            ),
        });
    }
    if scanned > 0 {
        cache.prune(dir, &seen);
    }
    // 启用的排前面，再按名字排：和"文件夹里一眼扫过去"的习惯一致
    out.sort_by(|a, b| {
        b.enabled.cmp(&a.enabled).then_with(|| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        })
    });
    Ok(out)
}

// ---------------------------------------------------------------------------
// 资源包
// ---------------------------------------------------------------------------

/// `options.txt` 里的 `resourcePacks` 列表。返回值第二项是"文件存在吗"。
fn read_options_resource_packs(game_dir: &Path) -> ApiResult<(Vec<String>, bool)> {
    let path = game_dir.join("options.txt");
    if !path.is_file() {
        return Ok((Vec::new(), false));
    }
    let text =
        fs::read_to_string(&path).map_err(|e| ApiError::io(format!("{}: {e}", path.display())))?;
    Ok((parse_options_resource_packs(&text)?, true))
}

fn parse_options_resource_packs(text: &str) -> ApiResult<Vec<String>> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("resourcePacks:") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            return Ok(Vec::new());
        }
        // 游戏写的是 `["vanilla","file/xxx.zip"]`
        return serde_json::from_str::<Vec<String>>(rest)
            .map_err(|e| ApiError::Format(format!("options.txt 的 resourcePacks 读不出来：{e}")));
    }
    Ok(Vec::new())
}

fn read_packs(dir: &Path, game_dir: &Path, names: &ModNames) -> ApiResult<Vec<LocalPack>> {
    let (listed, _) = read_options_resource_packs(game_dir)?;
    let is_enabled = |file_name: &str| {
        let key = format!("file/{file_name}");
        listed.iter().any(|item| item.eq_ignore_ascii_case(&key))
    };

    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (file_name, entry) in read_dir_sorted(dir)? {
        let Ok(meta) = entry.metadata() else { continue };
        let is_dir = meta.is_dir();
        if !is_dir && !file_name.to_lowercase().ends_with(".zip") {
            continue;
        }
        let stem = file_name
            .strip_suffix(".zip")
            .or_else(|| file_name.strip_suffix(".ZIP"))
            .unwrap_or(&file_name);
        let enabled = is_enabled(&file_name);
        seen.push(file_name.clone());
        out.push(LocalPack {
            display_name: names
                .translated(stem, None)
                .unwrap_or_else(|| stem.to_string()),
            file_name,
            size_bytes: if is_dir { 0 } else { meta.len() },
            enabled,
            missing: false,
            is_dir,
            modified_at: modified_secs(&meta),
        });
    }

    // options.txt 里登记了、磁盘上却没有的项：让用户能看见并清掉
    for item in &listed {
        let Some(name) = item.strip_prefix("file/") else {
            continue; // vanilla / programmer_art / mod_resources 不是文件
        };
        if seen.iter().any(|f| f.eq_ignore_ascii_case(name)) {
            continue;
        }
        let stem = name.strip_suffix(".zip").unwrap_or(name);
        out.push(LocalPack {
            display_name: stem.to_string(),
            file_name: name.to_string(),
            size_bytes: 0,
            enabled: true,
            missing: true,
            is_dir: false,
            modified_at: None,
        });
    }

    out.sort_by(|a, b| {
        b.enabled.cmp(&a.enabled).then_with(|| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        })
    });
    Ok(out)
}

// ---------------------------------------------------------------------------
// 存档
// ---------------------------------------------------------------------------

fn game_mode_label(data: &serde_json::Value) -> String {
    let hardcore = data.get("hardcore").and_then(|v| v.as_i64()).unwrap_or(0) != 0;
    if hardcore {
        return "hardcore".to_string();
    }
    match data.get("GameType").and_then(|v| v.as_i64()).unwrap_or(-1) {
        0 => "survival",
        1 => "creative",
        2 => "adventure",
        3 => "spectator",
        _ => "unknown",
    }
    .to_string()
}

fn dir_size(dir: &Path) -> (u64, bool) {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut capped = false;
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(SAVE_WALK_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
        seen += 1;
        if seen >= SAVE_WALK_LIMIT {
            capped = true;
            break;
        }
    }
    (total, capped)
}

fn read_saves(dir: &Path) -> ApiResult<Vec<LocalSave>> {
    let mut out = Vec::new();
    for (folder, entry) in read_dir_sorted(dir)? {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let world = entry.path();
        if !world.join("level.dat").is_file() {
            continue;
        }
        // level.dat 坏了也要列出来：那是用户的存档，藏起来更糟
        let data = nbt::read_level_dat(&world.join("level.dat"))
            .ok()
            .and_then(|v| v.get("Data").cloned())
            .unwrap_or(serde_json::Value::Null);
        let name = data
            .get("LevelName")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| folder.clone());
        let mc_version = data
            .get("Version")
            .and_then(|v| v.get("Name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let last_played = data
            .get("LastPlayed")
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0)
            // NBT 里的 `LastPlayed` 是 epoch **毫秒**，而 `content` 里其它时间
            // （`modified_at` 之类）一律是**秒**。在边界上换算掉，别把这个坑
            // 丢给界面——界面按秒解释毫秒，年份会变成 5 万年后。
            .map(|ms| ms / 1000);
        let (size_bytes, size_capped) = dir_size(&world);
        out.push(LocalSave {
            folder,
            name,
            game_mode: game_mode_label(&data),
            mc_version,
            last_played,
            size_bytes,
            size_capped,
            has_icon: world.join("icon.png").is_file(),
        });
    }
    out.sort_by(|a, b| {
        b.last_played
            .unwrap_or(0)
            .cmp(&a.last_played.unwrap_or(0))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

// ---------------------------------------------------------------------------
// 日志
// ---------------------------------------------------------------------------

fn read_logs(dir: &Path) -> ApiResult<Vec<LogFile>> {
    let mut out = Vec::new();
    for (file_name, entry) in read_dir_sorted(dir)? {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let lower = file_name.to_lowercase();
        let compressed = lower.ends_with(".log.gz");
        if !compressed && !lower.ends_with(".log") {
            continue;
        }
        out.push(LogFile {
            is_latest: lower == "latest.log",
            compressed,
            file_name,
            size_bytes: meta.len(),
            modified_at: modified_secs(&meta),
        });
    }
    out.sort_by(|a, b| {
        b.is_latest
            .cmp(&a.is_latest)
            .then_with(|| b.modified_at.unwrap_or(0).cmp(&a.modified_at.unwrap_or(0)))
            .then_with(|| a.file_name.cmp(&b.file_name))
    });
    Ok(out)
}

struct Tail {
    text: String,
    total: u64,
    shown: u64,
    truncated: bool,
}

fn read_tail(path: &Path, max_bytes: usize) -> ApiResult<Tail> {
    let mut file =
        fs::File::open(path).map_err(|e| ApiError::io(format!("{}: {e}", path.display())))?;
    let total = file
        .metadata()
        .map_err(|e| ApiError::io(format!("{}: {e}", path.display())))?
        .len();
    let take = (max_bytes as u64).min(total);
    if take > 0 {
        file.seek(SeekFrom::Start(total - take))
            .map_err(|e| ApiError::io(format!("{}: {e}", path.display())))?;
    }
    let mut buf = Vec::with_capacity(take as usize);
    (&mut file)
        .take(take)
        .read_to_end(&mut buf)
        .map_err(|e| ApiError::io(format!("{}: {e}", path.display())))?;
    Ok(Tail {
        text: String::from_utf8_lossy(&buf).into_owned(),
        total,
        shown: buf.len() as u64,
        truncated: total > take,
    })
}

fn read_tail_gz(path: &Path, max_bytes: usize) -> ApiResult<Tail> {
    let raw = fs::read(path).map_err(|e| ApiError::io(format!("{}: {e}", path.display())))?;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(raw.as_slice())
        .take(LOG_INFLATE_MAX)
        .read_to_end(&mut out)
        .map_err(|e| ApiError::Format(format!("无法解压 {}：{e}", path.display())))?;
    let total = out.len() as u64;
    let capped = total >= LOG_INFLATE_MAX;
    let take = max_bytes.min(out.len());
    let slice = &out[out.len() - take..];
    Ok(Tail {
        text: String::from_utf8_lossy(slice).into_owned(),
        total,
        shown: take as u64,
        truncated: capped || take < out.len(),
    })
}

// ---------------------------------------------------------------------------
// 模组图标
// ---------------------------------------------------------------------------

/// 图标条目名 → MIME。
///
/// 走白名单而不是"猜 `image/xxx`"：`icon` 字段可以是 jar 里任意条目，把
/// `assets/x/icon.txt` 当图片塞进 data URL 只会让界面显示一个破图。
pub(crate) fn icon_mime(entry: &str) -> Option<&'static str> {
    // 只看文件名那一段：`assets/mod.icon/icon.png` 这种路径里的点不是扩展名。
    let name = entry.rsplit('/').next().unwrap_or(entry);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// 拼 base64 data URL。
pub(crate) fn icon_data_url(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

// ---------------------------------------------------------------------------
// 更新检查
// ---------------------------------------------------------------------------

/// 待检查的一个已装文件。
struct InstalledMod {
    file_name: String,
    path: PathBuf,
    /// jar 里声明的 mod id（图个显示用，不影响查更新）。
    mod_id: Option<String>,
    /// jar 里声明的版本；读不出来就 `None`（比较时退化为"只看文件是否相同"）。
    current_version: Option<String>,
    /// 文件内容 SHA-1：Modrinth 的「按哈希查版本」接口就吃这个。
    sha1: String,
}

/// 某个项目在「当前游戏版本 + 加载器」下最新的那个文件。
struct LatestFile {
    source: ModSource,
    /// Modrinth 的 project_id / CurseForge 的 modId。
    project_id: String,
    /// 站点上的版本号（对比的"目标版本"）。
    version: String,
    file_name: String,
    url: String,
    size: u64,
    sha1: Option<String>,
    page_url: Option<String>,
}

/// 一条「去查这个项目的最新文件」任务。
struct LookupTask {
    /// 对应待检查列表里的下标：结果要回填到那个文件上。
    index: usize,
    source: ModSource,
    project_id: String,
}

/// `mods/` 里是否算一个模组文件（启用或停用都算）。
fn is_mod_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".jar") || lower.ends_with(".jar.disabled")
}

/// 更新检查用的 agent：UA 必须带 `KMLH/<版本>`（MCIM 按它放行），
/// 读写超时都比整次检查的预算短，保证一条慢请求吃不掉全部时间。
fn update_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(UPDATE_READ_TIMEOUT)
        .user_agent(mods::USER_AGENT)
        .build()
}

/// 依次试候选地址 GET 出 JSON：官方挂了自动走镜像。
fn get_json_candidates(agent: &ureq::Agent, urls: &[String]) -> Result<Value, String> {
    let mut last = String::new();
    for url in urls {
        match agent.get(url).call() {
            Ok(response) => match response.into_json::<Value>() {
                Ok(value) => return Ok(value),
                Err(err) => last = format!("响应不是 JSON：{err}"),
            },
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                let short: String = body.trim().chars().take(120).collect();
                last = format!("HTTP {code}：{short}");
            }
            Err(err) => last = err.to_string(),
        }
    }
    Err(if last.is_empty() {
        "没有可用的接口地址".to_string()
    } else {
        last
    })
}

/// 同上，但发 POST JSON。
fn post_json_candidates(agent: &ureq::Agent, urls: &[String], body: &Value) -> Result<Value, String> {
    let mut last = String::new();
    for url in urls {
        match agent.post(url).send_json(body.clone()) {
            Ok(response) => match response.into_json::<Value>() {
                Ok(value) => return Ok(value),
                Err(err) => last = format!("响应不是 JSON：{err}"),
            },
            Err(ureq::Error::Status(code, response)) => {
                let text = response.into_string().unwrap_or_default();
                let short: String = text.trim().chars().take(120).collect();
                last = format!("HTTP {code}：{short}");
            }
            Err(err) => last = err.to_string(),
        }
    }
    Err(if last.is_empty() {
        "没有可用的接口地址".to_string()
    } else {
        last
    })
}

/// 一次问完所有 SHA-1：Modrinth 返回 `hash → 版本对象`。
fn modrinth_match_hashes(
    agent: &ureq::Agent,
    policy: &str,
    hashes: &[String],
) -> Result<Value, String> {
    let body = json!({ "hashes": hashes, "algorithm": "sha1" });
    let urls = mods::candidate_urls(policy, ModSource::Modrinth, "/version_files");
    post_json_candidates(agent, &urls, &body)
}

/// 某个 Modrinth 项目在「游戏版本 + 加载器」下最新的文件。
///
/// 用 `/project/{id}/version` 而不是直接相信哈希查到的那个版本：用户装的可能是
/// 老版本，我们要的是"最新可用"；筛选条件由站点做，绝不会把别的加载器的文件
/// 当成更新推给用户。
fn modrinth_latest_file(
    agent: &ureq::Agent,
    policy: &str,
    project_id: &str,
    game: &str,
    loader: Option<&str>,
) -> Option<LatestFile> {
    let mut path = format!("/project/{project_id}/version?limit=50");
    if !game.trim().is_empty() {
        path.push_str(&format!(
            "&game_versions={}",
            mods::percent_encode(&format!(r#"["{}"]"#, game.trim()))
        ));
    }
    if let Some(loader) = loader.and_then(normalize_loader) {
        path.push_str(&format!(
            "&loaders={}",
            mods::percent_encode(&format!(r#"["{loader}"]"#))
        ));
    }
    let urls = mods::candidate_urls(policy, ModSource::Modrinth, &path);
    let value = get_json_candidates(agent, &urls).ok()?;
    let items = value.as_array()?;
    // 接口一般已按发布时间倒序，但不能指望——显式取 `date_published` 最大的那个。
    let mut best: Option<(&Value, &Value, &str)> = None;
    for item in items {
        let Some(file) = mods::primary_file(item) else {
            continue;
        };
        let date = item
            .get("date_published")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let better = best
            .as_ref()
            .map(|(_, _, current)| date > *current)
            .unwrap_or(true);
        if better {
            best = Some((item, file, date));
        }
    }
    let (item, file, _) = best?;
    let version = item
        .get("version_number")
        .and_then(|v| v.as_str())?
        .to_string();
    let file_name = file.get("filename").and_then(|v| v.as_str())?.to_string();
    let url = file.get("url").and_then(|v| v.as_str())?.to_string();
    if url.is_empty() || file_name.is_empty() {
        return None;
    }
    Some(LatestFile {
        source: ModSource::Modrinth,
        project_id: project_id.to_string(),
        version,
        file_name,
        url,
        size: file.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        sha1: file
            .get("hashes")
            .and_then(|h| h.get("sha1"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        // 只有项目 id：`modrinth.com/mod/<id>` 会跳到 slug 页，够用户去看更新日志了。
        page_url: Some(format!("https://modrinth.com/mod/{project_id}")),
    })
}

/// CurseForge 的 `modLoaderType` 枚举（1 forge / 4 fabric / 5 quilt / 6 neoforge）。
fn cf_loader_type(loader: &str) -> Option<i64> {
    match normalize_loader(loader)? {
        "forge" => Some(1),
        "fabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

/// 用 CurseForge 指纹批量认文件，返回 `指纹 → modId`。
///
/// CurseForge 不按 SHA-1 认文件，所以 Modrinth 匹配不到的模组才走这里兜底。
fn curseforge_match_fingerprints(
    agent: &ureq::Agent,
    policy: &str,
    fingerprints: &[u32],
) -> Result<HashMap<u32, i64>, String> {
    let body = json!({ "fingerprints": fingerprints });
    let urls = mods::candidate_urls(policy, ModSource::CurseForge, "/fingerprints");
    let value = post_json_candidates(agent, &urls, &body)?;
    let mut out = HashMap::new();
    let Some(matches) = value.pointer("/data/exactMatches").and_then(|v| v.as_array()) else {
        return Ok(out);
    };
    for item in matches {
        // `exactMatches[].id` 就是命中的那个指纹，`file.modId` 是项目 id。
        let Some(print) = item.get("id").and_then(|v| v.as_i64()) else {
            continue;
        };
        let Some(mod_id) = item.pointer("/file/modId").and_then(|v| v.as_i64()) else {
            continue;
        };
        out.insert(print as u32, mod_id);
    }
    Ok(out)
}

/// 某个 CurseForge 项目在「游戏版本 + 加载器」下最新的文件。
fn curseforge_latest_file(
    agent: &ureq::Agent,
    policy: &str,
    mod_id: i64,
    game: &str,
    loader: Option<&str>,
) -> Option<LatestFile> {
    let mut path = format!("/mods/{mod_id}/files?pageSize=50");
    if !game.trim().is_empty() {
        path.push_str(&format!(
            "&gameVersion={}",
            mods::percent_encode(game.trim())
        ));
    }
    if let Some(kind) = loader.and_then(cf_loader_type) {
        path.push_str(&format!("&modLoaderType={kind}"));
    }
    let urls = mods::candidate_urls(policy, ModSource::CurseForge, &path);
    let value = get_json_candidates(agent, &urls).ok()?;
    let files = value.get("data")?.as_array()?;
    // CF 不保证按时间排序（`sortField` 在 files 接口上不可用），自己挑最新的。
    let best = files.iter().max_by_key(|file| {
        file.get("fileDate")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    })?;
    let file_id = best.get("id").and_then(|v| v.as_i64())?;
    let file_name = best.get("fileName").and_then(|v| v.as_str())?.to_string();
    if file_name.is_empty() {
        return None;
    }
    // CF 的文件对象没有独立的版本号字段：`displayName`（"JEI 15.2.0.27 for 1.21.1"）
    // 是唯一带语义的字符串，没有它就只能拿文件名当版本，至少能看出变化。
    let version = best
        .get("displayName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(&file_name)
        .to_string();
    let mut url = best
        .get("downloadUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if url.is_empty() {
        // 作者关掉第三方分发时 `downloadUrl` 是 null，得再问一次专用接口。
        let path = format!("/mods/{mod_id}/files/{file_id}/download-url");
        let urls = mods::candidate_urls(policy, ModSource::CurseForge, &path);
        if let Ok(value) = get_json_candidates(agent, &urls) {
            url = value
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
    }
    if url.is_empty() {
        return None;
    }
    let sha1 = best
        .get("hashes")
        .and_then(|v| v.as_array())
        .and_then(|list| {
            list.iter().find(|hash| {
                hash.get("algo")
                    .and_then(|v| v.as_str())
                    .map(|algo| algo.eq_ignore_ascii_case("sha1"))
                    .unwrap_or(false)
            })
        })
        .and_then(|hash| hash.get("value"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(LatestFile {
        source: ModSource::CurseForge,
        project_id: mod_id.to_string(),
        version,
        file_name,
        url,
        size: best.get("fileLength").and_then(|v| v.as_u64()).unwrap_or(0),
        sha1,
        page_url: Some(format!("https://www.curseforge.com/projects/{mod_id}")),
    })
}

/// 并发跑「取最新文件」的查询，并守住总时限。
///
/// 为什么不用 `thread::scope`：scope 会 join 所有线程，一条卡住的请求就能把整次
/// 检查拖到预算之外。这里 channel + `recv_timeout`，到点就带着已有结果返回，
/// 没跑完的线程自己跑完结束（结果没人接收就丢弃）。
fn run_lookups(
    agent: &ureq::Agent,
    policy: &str,
    tasks: Vec<LookupTask>,
    game: &str,
    loader: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<(usize, LatestFile)> {
    let total = tasks.len();
    let mut done: Vec<(usize, LatestFile)> = Vec::new();
    if tasks.is_empty() {
        return done;
    }
    let workers = total.min(UPDATE_MAX_WORKERS);
    let deadline = Instant::now() + UPDATE_DEADLINE;
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Option<LatestFile>)>();

    let mut queues: Vec<Vec<LookupTask>> = (0..workers).map(|_| Vec::new()).collect();
    for (index, task) in tasks.into_iter().enumerate() {
        queues[index % workers].push(task);
    }
    for queue in queues {
        let tx = tx.clone();
        let agent = agent.clone();
        let policy = policy.to_string();
        let game = game.to_string();
        let loader = loader.map(str::to_string);
        // 线程起不来（极端资源不足）就当这条队列没跑：结果会少几条，最后有提示。
        let _ = std::thread::Builder::new()
            .name("kaleid-mod-update".into())
            .spawn(move || {
                for task in queue {
                    let latest = match task.source {
                        ModSource::Modrinth => modrinth_latest_file(
                            &agent,
                            &policy,
                            &task.project_id,
                            &game,
                            loader.as_deref(),
                        ),
                        ModSource::CurseForge => match task.project_id.parse::<i64>() {
                            Ok(mod_id) => curseforge_latest_file(
                                &agent,
                                &policy,
                                mod_id,
                                &game,
                                loader.as_deref(),
                            ),
                            // 项目 id 不是数字说明上游给了脏数据，跳过这个条目。
                            Err(_) => None,
                        },
                    };
                    if tx.send((task.index, latest)).is_err() {
                        break;
                    }
                }
            });
    }
    drop(tx);

    let mut timed_out = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            timed_out = true;
            break;
        }
        match rx.recv_timeout(left) {
            Ok((index, Some(latest))) => done.push((index, latest)),
            // 查不到（项目删了、没有适配当前版本的文件）：不算更新，也不算错误。
            Ok((_, None)) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if timed_out {
        mods::push_warning(
            warnings,
            format!(
                "更新检查超过 {} 秒，已先返回部分结果，稍后可再点一次",
                UPDATE_DEADLINE.as_secs()
            ),
        );
    } else if done.len() < total {
        mods::push_warning(
            warnings,
            format!("有 {} 个模组的更新信息没能取到", total - done.len()),
        );
    }
    done
}

/// 装上的是不是已经是最新：先看内容哈希，再看版本号。
///
/// 哈希相同就是同一个文件——比版本号可靠得多（同一个版本被重新打包很常见）。
/// 版本号只在"拿到元数据"时才有，读不出来就只能靠哈希判断。
fn decide_update(installed_sha1: &str, latest: &LatestFile, current: Option<&str>) -> bool {
    if latest
        .sha1
        .as_deref()
        .map(|sha1| sha1.eq_ignore_ascii_case(installed_sha1))
        .unwrap_or(false)
    {
        return false;
    }
    match current.map(str::trim).filter(|v| !v.is_empty()) {
        Some(current) => !current.eq_ignore_ascii_case(latest.version.trim()),
        // 不知道装的是哪个版本，但内容确实不同：交给用户决定要不要更。
        None => true,
    }
}

/// 备份目录名：`<epoch 秒>`，同一秒里第二次备份加序号，避免互相覆盖。
fn backup_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

/// 在目录里挑一个还没被占用的文件名：`foo.jar` → `foo (1).jar`。
///
/// 重名不能覆盖：`latest_file_name` 可能和用户自己丢进去的某个 jar 撞名。
pub(crate) fn unique_file_name(dir: &Path, wanted: &str) -> String {
    if !dir.join(wanted).exists() {
        return wanted.to_string();
    }
    let (stem, tail) = if let Some(base) = wanted.strip_suffix(".jar.disabled") {
        (base, ".jar.disabled")
    } else if let Some(base) = wanted.strip_suffix(".jar") {
        (base, ".jar")
    } else {
        (wanted, "")
    };
    for n in 1..1000 {
        let candidate = format!("{stem} ({n}){tail}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    // 极端情况（同名文件上千个）：退化成带时间戳的名字，绝不覆盖。
    format!("{stem}-{}{tail}", backup_stamp())
}

// ---------------------------------------------------------------------------
// 服务实现
// ---------------------------------------------------------------------------

impl ContentService {
    /// 取某个已装模组的图标（base64 data URL）。
    ///
    /// 只在界面真的要显示图标时才调用：图标数据走 base64 会撑大桥接消息，
    /// 列表里只给 `has_icon` 这个布尔值，用户滚动到哪一行再取哪一行的图。
    pub fn mod_icon(&self, root_id: &str, version_id: &str, file_name: &str) -> ApiResult<ModIcon> {
        let dir = self.area_dir(root_id, version_id, ContentKind::Mod)?;
        let file_name = plain_name(file_name)?;
        let path = dir.join(file_name);
        let meta = fs::metadata(&path)
            .map_err(|_| ApiError::not_found(format!("找不到模组：{file_name}")))?;
        if !meta.is_file() {
            return Err(ApiError::not_found(format!("不是文件：{file_name}")));
        }
        let jar_meta = self.mod_meta.load(
            &dir,
            file_name,
            &path,
            modified_secs(&meta).unwrap_or(0),
            meta.len(),
        );
        let entry = jar_meta
            .and_then(|meta| meta.icon_entry)
            .ok_or_else(|| ApiError::not_found(format!("{file_name} 里没有声明图标")))?;
        // 扩展名先过一遍：不是图片就不必去解压它了。
        let mime = icon_mime(&entry)
            .ok_or_else(|| ApiError::not_found(format!("不支持的图标格式：{entry}")))?;
        let bytes = jar::read_entry_bytes(&path, &entry, MAX_ICON_BYTES)
            .ok_or_else(|| ApiError::not_found(format!("图标读不出来或超过 512 KiB：{entry}")))?;
        if bytes.is_empty() {
            return Err(ApiError::not_found(format!("图标是空文件：{entry}")));
        }
        Ok(ModIcon {
            data_url: icon_data_url(mime, &bytes),
            mime: mime.to_string(),
        })
    }

    /// 检查 `mods/` 里每个模组有没有可用更新。
    ///
    /// 流程：算 SHA-1 → Modrinth 批量认文件 → 取该项目在「当前游戏版本 + 加载器」
    /// 下的最新文件 → 与已装版本比较。Modrinth 认不出的再用 CurseForge 指纹兜底；
    /// 两边都认不出就进 `unknown`（**不是错误**：很多模组只发在 GitHub 上）。
    pub fn check_updates(&self, root_id: &str, version_id: &str) -> ApiResult<ModUpdateReport> {
        let dir = self.area_dir(root_id, version_id, ContentKind::Mod)?;
        let (game, loader) = self.game_and_loader(root_id, version_id)?;
        let mut report = ModUpdateReport {
            updates: Vec::new(),
            unknown: Vec::new(),
            warnings: Vec::new(),
            checked_at: mods::now_ms(),
        };
        let installed = self.installed_for_update(&dir, &mut report.warnings);
        if installed.is_empty() {
            return Ok(report);
        }

        let agent = update_agent();
        let policy = mods::mod_policy(&self.settings);
        // `vanilla` / `unrecognized` 传进去会被 `normalize_loader` 过滤掉，所以直接给 slug。
        let loader_slug = loader.slug();
        let loader_ref = Some(loader_slug).filter(|slug| normalize_loader(slug).is_some());

        // 1) Modrinth：一次请求问完全部 SHA-1。
        let hashes: Vec<String> = installed.iter().map(|item| item.sha1.clone()).collect();
        let mut matched: HashSet<usize> = HashSet::new();
        let mut tasks: Vec<LookupTask> = Vec::new();
        match modrinth_match_hashes(&agent, &policy, &hashes) {
            Ok(value) => {
                for (index, item) in installed.iter().enumerate() {
                    let Some(version) = value.get(&item.sha1) else {
                        continue;
                    };
                    let Some(project) = version.get("project_id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    matched.insert(index);
                    tasks.push(LookupTask {
                        index,
                        source: ModSource::Modrinth,
                        project_id: project.to_string(),
                    });
                }
            }
            Err(err) => mods::push_warning(&mut report.warnings, format!("Modrinth 查版本失败：{err}")),
        }

        // 2) CurseForge：用归一化后的内容指纹认文件（它不认 SHA-1）。
        let mut by_print: HashMap<u32, usize> = HashMap::new();
        for (index, item) in installed.iter().enumerate() {
            if matched.contains(&index) {
                continue;
            }
            if let Some(print) = crate::fingerprint::fingerprint_file(&item.path) {
                by_print.insert(print, index);
            }
        }
        if !by_print.is_empty() {
            let prints: Vec<u32> = by_print.keys().copied().collect();
            match curseforge_match_fingerprints(&agent, &policy, &prints) {
                Ok(found) => {
                    for (print, mod_id) in found {
                        let Some(&index) = by_print.get(&print) else {
                            continue;
                        };
                        matched.insert(index);
                        tasks.push(LookupTask {
                            index,
                            source: ModSource::CurseForge,
                            project_id: mod_id.to_string(),
                        });
                    }
                }
                Err(err) => {
                    mods::push_warning(&mut report.warnings, format!("CurseForge 查指纹失败：{err}"))
                }
            }
        }

        // 3) 并发查每个项目的最新文件（有总时限，超时先返回已有结果）。
        let found = run_lookups(
            &agent,
            &policy,
            tasks,
            &game,
            loader_ref,
            &mut report.warnings,
        );
        for (index, file) in found {
            let item = &installed[index];
            if !decide_update(&item.sha1, &file, item.current_version.as_deref()) {
                continue;
            }
            report.updates.push(ModUpdate {
                file_name: item.file_name.clone(),
                mod_id: item.mod_id.clone(),
                source: file.source.as_str().to_string(),
                project_id: file.project_id,
                current_version: item.current_version.clone(),
                latest_version: file.version,
                latest_file_name: file.file_name,
                download_url: file.url,
                size: file.size,
                sha1: file.sha1,
                page_url: file.page_url,
            });
        }
        // 认不出来源的文件：文件名给界面列出来，用户自己去项目主页看。
        for (index, item) in installed.iter().enumerate() {
            if !matched.contains(&index) {
                report.unknown.push(item.file_name.clone());
            }
        }
        report.updates.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        report.unknown.sort();
        Ok(report)
    }

    /// 把某个更新装到 `mods/`。
    ///
    /// 顺序是「下载到暂存名 → 备份旧文件 → 改名就位」，任何一步失败都不会留下
    /// 半截 jar 或让旧文件凭空消失：
    /// - 先下到 `.kaleid-update-*.tmp`：下载器写一半失败也不会被游戏当成模组加载；
    /// - 旧文件（含 `.disabled` 变体）搬进 `mods/.kaleid-backup/<时间戳>/` 而不是直接删，
    ///   用户想回滚时文件还在；
    /// - 新文件是否 `.disabled` 跟随旧文件的启用状态，别把用户故意停用的模组又启用回来。
    pub fn apply_update(
        &self,
        root_id: &str,
        version_id: &str,
        update: &ModUpdate,
    ) -> ApiResult<()> {
        let downloads = self.downloads.clone().ok_or_else(|| {
            ApiError::Unavailable("下载服务不可用，请先启用「版本管理」插件".into())
        })?;
        let dir = self.area_dir(root_id, version_id, ContentKind::Mod)?;
        fs::create_dir_all(&dir).map_err(|e| ApiError::io(format!("{}: {e}", dir.display())))?;

        let old_name = plain_name(&update.file_name)?.to_string();
        if !is_mod_file(&old_name) {
            return Err(ApiError::invalid(format!("不是模组文件：{old_name}")));
        }
        let enabled = !old_name.ends_with(".disabled");
        let new_name = plain_name(&update.latest_file_name)?.to_string();
        if !is_mod_file(&new_name) {
            return Err(ApiError::invalid(format!(
                "更新文件名不是 jar：{new_name}"
            )));
        }
        if update.download_url.trim().is_empty() {
            return Err(ApiError::invalid("这条更新没有下载地址"));
        }

        let staging = dir.join(format!(".kaleid-update-{}.tmp", uuid::Uuid::new_v4()));
        let policy = mods::mod_policy(&self.settings);
        let (game, loader) = self
            .game_and_loader(root_id, version_id)
            .unwrap_or_else(|_| (version_id.to_string(), LoaderKind::Vanilla));
        let job = Job::with_initial_source(
            uuid::Uuid::new_v4().to_string(),
            format!("更新模组 {}", update.latest_file_name),
            version_id.to_string(),
            root_id.to_string(),
            game,
            loader,
            None,
            mods::job_policy(&policy),
            // 队列里显示「MCIM」：模组文件只从 Modrinth / 镜像取，和 BMCLAPI 无关。
            "mcim",
        );
        downloads.register(job.clone());
        job.set_status(DownloadStatus::Queued, "准备下载更新");
        job.add_total("mods", update.size, 1);

        // CF 的文件直链在 edge.forgecdn.net，国内常连不上：官方地址只给 Modrinth，
        // 镜像地址由 `mirror_url_for` 换算（换不出来就用原地址当唯一的镜像）。
        let (official_url, mirror_url) = if update.source.eq_ignore_ascii_case("modrinth") {
            (
                Some(update.download_url.clone()),
                mirror_url_for(&update.download_url),
            )
        } else {
            (
                None,
                mirror_url_for(&update.download_url).or_else(|| Some(update.download_url.clone())),
            )
        };
        let item = DownloadItem {
            label: update.latest_file_name.clone(),
            phase: "mods".to_string(),
            official_url,
            mirror_url,
            fallback_url: None,
            dest: staging.clone(),
            size: update.size,
            sha1: update.sha1.clone(),
        };
        if let Err(err) = downloads.run_batch(job.clone(), vec![item]) {
            // 半截文件留在 `mods/` 里会被游戏当成模组装进去，必须清掉。
            let _ = fs::remove_file(&staging);
            return Err(err);
        }
        job.complete_phase("mods");
        job.complete();

        // 备份旧文件：本体和它的 `.disabled` 变体都可能存在（用户两种都留过）。
        let stem = old_name
            .strip_suffix(".jar.disabled")
            .or_else(|| old_name.strip_suffix(".jar"))
            .unwrap_or(&old_name);
        let backup_dir = dir.join(BACKUP_DIR).join(backup_stamp());
        let mut moved = 0usize;
        for name in [
            format!("{stem}.jar"),
            format!("{stem}.jar.disabled"),
        ] {
            let source = dir.join(&name);
            if !source.is_file() {
                continue;
            }
            if moved == 0 {
                fs::create_dir_all(&backup_dir)
                    .map_err(|e| ApiError::io(format!("{}: {e}", backup_dir.display())))?;
            }
            let dest = backup_dir.join(&name);
            fs::rename(&source, &dest).map_err(|e| {
                ApiError::io(format!(
                    "旧模组移到备份目录失败（游戏可能正占用文件）：{e}"
                ))
            })?;
            moved += 1;
        }

        // 旧文件已经让位，这时再挑新名字就不可能撞上它。
        let target_name = if enabled {
            new_name.clone()
        } else {
            format!("{new_name}.disabled")
        };
        let target = dir.join(unique_file_name(&dir, &target_name));
        fs::rename(&staging, &target).map_err(|e| {
            ApiError::io(format!(
                "新模组就位失败：{e}（旧文件已备份到 {}）",
                backup_dir.display()
            ))
        })?;
        Ok(())
    }

    /// 这次更新检查对应的「游戏版本 + 加载器」。
    fn game_and_loader(&self, root_id: &str, version_id: &str) -> ApiResult<(String, LoaderKind)> {
        let versions = self.versions.as_ref().ok_or_else(|| {
            ApiError::Unavailable("版本扫描服务不可用，无法确定要检查哪个游戏版本".into())
        })?;
        let root = self.roots.get(root_id)?;
        let json_path = root
            .versions_dir()
            .join(version_id)
            .join(format!("{version_id}.json"));
        let summary = versions
            .inspect(&json_path)
            .map_err(|_| ApiError::not_found(format!("读不出版本 {version_id} 的 JSON")))?;
        // 用的就是「热门」列表那套判断：沿 `inheritsFrom` 找到原版版本号。
        // 不能直接拿版本 id，`1.21.4-fabric` 这种在 Modrinth 上什么都查不到。
        let game = mods::resolve_game_version(versions, &root, &summary);
        Ok((game, summary.loader))
    }

    /// 列出待检查的模组：算 SHA-1、读 jar 里声明的版本。
    ///
    /// SHA-1 在内存里算（`mods::file_sha1`），不往磁盘写任何哈希缓存：一次检查
    /// 就几十个文件，落盘反而要额外处理失效。
    fn installed_for_update(
        &self,
        dir: &Path,
        warnings: &mut Vec<String>,
    ) -> Vec<InstalledMod> {
        let Ok(entries) = read_dir_sorted(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (file_name, entry) in entries {
            if !is_mod_file(&file_name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(sha1) = mods::file_sha1(&path) else {
                mods::push_warning(
                    warnings,
                    format!("读不出 {file_name} 的内容，已跳过它的更新检查"),
                );
                continue;
            };
            let jar_meta = self.mod_meta.load(
                dir,
                &file_name,
                &path,
                modified_secs(&meta).unwrap_or(0),
                meta.len(),
            );
            out.push(InstalledMod {
                mod_id: jar_meta.as_ref().and_then(|meta| meta.mod_id.clone()),
                current_version: jar_meta.as_ref().and_then(|meta| meta.version.clone()),
                path,
                file_name,
                sha1,
            });
        }
        out
    }
}

impl ContentServiceApi for ContentService {
    fn dirs(&self, root_id: &str, version_id: &str) -> ApiResult<ContentDirs> {
        let (game_dir, isolated, isolated_from) = self.game_dir(root_id, version_id)?;
        let join = |name: &str| game_dir.join(name).to_string_lossy().to_string();
        Ok(ContentDirs {
            game_dir: game_dir.to_string_lossy().to_string(),
            mods_dir: join("mods"),
            resource_packs_dir: join("resourcepacks"),
            saves_dir: join("saves"),
            logs_dir: join("logs"),
            isolated,
            isolated_from,
        })
    }

    fn mods(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LocalMod>> {
        let dir = self.area_dir(root_id, version_id, ContentKind::Mod)?;
        read_mods(&dir, &self.names, &self.mod_meta)
    }

    fn set_mod_enabled(
        &self,
        root_id: &str,
        version_id: &str,
        file: &str,
        enabled: bool,
    ) -> ApiResult<Vec<LocalMod>> {
        let dir = self.area_dir(root_id, version_id, ContentKind::Mod)?;
        let file = plain_name(file)?;
        let (stem, currently_enabled) = if let Some(base) = file.strip_suffix(".jar.disabled") {
            (base, false)
        } else if let Some(base) = file.strip_suffix(".jar") {
            (base, true)
        } else {
            return Err(ApiError::invalid(format!("不是模组文件：{file}")));
        };
        let source = dir.join(file);
        if !source.is_file() {
            return Err(ApiError::not_found(format!("找不到模组：{file}")));
        }
        if currently_enabled != enabled {
            let target_name = if enabled {
                format!("{stem}.jar")
            } else {
                format!("{stem}.jar.disabled")
            };
            let target = dir.join(&target_name);
            if target.exists() {
                return Err(ApiError::invalid(format!("目标文件已存在：{target_name}")));
            }
            fs::rename(&source, &target)
                .map_err(|e| ApiError::io(format!("改文件名失败（游戏可能正占用该文件）：{e}")))?;
        }
        read_mods(&dir, &self.names, &self.mod_meta)
    }

    fn resource_packs(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LocalPack>> {
        let (game_dir, _, _) = self.game_dir(root_id, version_id)?;
        read_packs(&game_dir.join("resourcepacks"), &game_dir, &self.names)
    }

    fn saves(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LocalSave>> {
        let (game_dir, _, _) = self.game_dir(root_id, version_id)?;
        read_saves(&game_dir.join("saves"))
    }

    fn logs(&self, root_id: &str, version_id: &str) -> ApiResult<Vec<LogFile>> {
        let (game_dir, _, _) = self.game_dir(root_id, version_id)?;
        read_logs(&game_dir.join("logs"))
    }

    fn read_log(
        &self,
        root_id: &str,
        version_id: &str,
        file: &str,
        max_bytes: usize,
    ) -> ApiResult<LogTail> {
        let (game_dir, _, _) = self.game_dir(root_id, version_id)?;
        let file = plain_name(file)?;
        let path = game_dir.join("logs").join(file);
        if !path.is_file() {
            return Err(ApiError::not_found(format!("找不到日志：{file}")));
        }
        let limit = max_bytes.clamp(1, LOG_TAIL_MAX);
        let tail = if file.to_lowercase().ends_with(".log.gz") {
            read_tail_gz(&path, limit)?
        } else {
            read_tail(&path, limit)?
        };
        Ok(LogTail {
            file_name: file.to_string(),
            total_bytes: tail.total,
            shown_bytes: tail.shown,
            truncated: tail.truncated,
            text: tail.text,
        })
    }

    fn import(
        &self,
        root_id: &str,
        version_id: &str,
        kind: ContentKind,
        paths: &[PathBuf],
    ) -> ApiResult<ImportOutcome> {
        let dir = self.area_dir(root_id, version_id, kind)?;
        fs::create_dir_all(&dir).map_err(|e| ApiError::io(format!("{}: {e}", dir.display())))?;
        let mut outcome = ImportOutcome::default();
        for path in paths {
            let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                outcome.skipped.push(ImportSkipped {
                    file: path.to_string_lossy().to_string(),
                    reason: "不是文件".into(),
                });
                continue;
            };
            if !path.is_file() {
                outcome.skipped.push(ImportSkipped {
                    file: file_name,
                    reason: "不是文件".into(),
                });
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !kind.extensions().iter().any(|want| *want == ext) {
                outcome.skipped.push(ImportSkipped {
                    file: file_name,
                    reason: format!("只接受 .{} 文件", kind.extensions().join(" / .")),
                });
                continue;
            }
            let target = dir.join(&file_name);
            if target.exists() {
                outcome.skipped.push(ImportSkipped {
                    file: file_name,
                    reason: "同名文件已存在（不覆盖）".into(),
                });
                continue;
            }
            match fs::copy(path, &target) {
                Ok(_) => outcome.copied.push(file_name),
                Err(e) => outcome.skipped.push(ImportSkipped {
                    file: file_name,
                    reason: format!("复制失败：{e}"),
                }),
            }
        }
        Ok(outcome)
    }

    fn delete(
        &self,
        root_id: &str,
        version_id: &str,
        kind: ContentKind,
        file: &str,
    ) -> ApiResult<()> {
        let dir = self.area_dir(root_id, version_id, kind)?;
        let file = plain_name(file)?;
        let path = dir.join(file);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            return Err(ApiError::not_found(format!(
                "找不到{}：{file}",
                kind.label_zh()
            )));
        };
        let result = if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.map_err(|e| {
            // 最常见的原因是游戏还开着、文件被占用：调用方要弹窗告警，别吞
            ApiError::io(format!("删除 {} 失败：{e}", kind.label_zh()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaleid_api::IsolationMode;

    struct Fixture {
        _dir: tempfile::TempDir,
        svc: ContentService,
        root_id: String,
        game_dir: PathBuf,
    }

    fn fixture(isolated: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let game = dir.path().join(".minecraft");
        fs::create_dir_all(game.join("versions/1.21.4-fabric")).unwrap();

        let roots = Arc::new(RootRegistry::empty_at(&data).unwrap());
        let root = roots.add(&game, Some("测试"), true).unwrap();
        roots
            .set_isolation(
                &root.id,
                if isolated {
                    IsolationMode::On
                } else {
                    IsolationMode::Off
                },
            )
            .unwrap();
        let settings = Arc::new(Settings::open(&data));
        let svc = ContentService::new(settings, roots.clone(), Arc::new(ModNames::default()));
        let game_dir = roots
            .get(&root.id)
            .unwrap()
            .game_dir("1.21.4-fabric", isolated);
        Fixture {
            _dir: dir,
            svc,
            root_id: root.id,
            game_dir,
        }
    }

    fn touch(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn game_dir_follows_isolation() {
        let iso = fixture(true);
        let dirs = iso.svc.dirs(&iso.root_id, "1.21.4-fabric").unwrap();
        assert!(dirs.isolated);
        assert!(dirs
            .mods_dir
            .replace('/', "\\")
            .ends_with("versions\\1.21.4-fabric\\mods"));
        assert_eq!(
            dirs.mods_dir,
            iso.game_dir.join("mods").to_string_lossy().to_string()
        );

        let shared = fixture(false);
        let dirs = shared.svc.dirs(&shared.root_id, "1.21.4-fabric").unwrap();
        assert!(!dirs.isolated);
        assert!(dirs.mods_dir.ends_with("mods"));
        assert!(!dirs.mods_dir.replace('/', "\\").contains("versions\\"));
        assert_eq!(dirs.game_dir, shared.game_dir.to_string_lossy().to_string());
    }

    #[test]
    fn mods_list_marks_disabled_jars() {
        let f = fixture(true);
        let mods = f.game_dir.join("mods");
        touch(&mods.join("sodium-fabric-0.6.9.jar"), b"aaa");
        touch(&mods.join("iris-1.8.1.jar.disabled"), b"bbbb");
        touch(&mods.join("readme.txt"), b"x");
        fs::create_dir_all(mods.join("subdir")).unwrap();

        let list = f.svc.mods(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(list.len(), 2, "{list:?}");
        assert_eq!(list[0].file_name, "sodium-fabric-0.6.9.jar");
        assert!(list[0].enabled);
        assert_eq!(list[0].display_name, "sodium-fabric");
        assert_eq!(list[0].version.as_deref(), Some("0.6.9"));
        assert_eq!(list[0].size_bytes, 3);
        assert_eq!(list[1].file_name, "iris-1.8.1.jar.disabled");
        assert!(!list[1].enabled);
    }

    #[test]
    fn missing_mods_dir_is_empty_not_error() {
        let f = fixture(true);
        assert!(f.svc.mods(&f.root_id, "1.21.4-fabric").unwrap().is_empty());
        assert!(f.svc.logs(&f.root_id, "1.21.4-fabric").unwrap().is_empty());
        assert!(f.svc.saves(&f.root_id, "1.21.4-fabric").unwrap().is_empty());
        assert!(f
            .svc
            .resource_packs(&f.root_id, "1.21.4-fabric")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn mod_toggle_renames_both_ways() {
        let f = fixture(true);
        let mods = f.game_dir.join("mods");
        touch(&mods.join("sodium-0.6.9.jar"), b"a");

        let list = f
            .svc
            .set_mod_enabled(&f.root_id, "1.21.4-fabric", "sodium-0.6.9.jar", false)
            .unwrap();
        assert!(!list[0].enabled);
        assert!(mods.join("sodium-0.6.9.jar.disabled").is_file());
        assert!(!mods.join("sodium-0.6.9.jar").exists());

        let list = f
            .svc
            .set_mod_enabled(
                &f.root_id,
                "1.21.4-fabric",
                "sodium-0.6.9.jar.disabled",
                true,
            )
            .unwrap();
        assert!(list[0].enabled);
        assert!(mods.join("sodium-0.6.9.jar").is_file());
    }

    #[test]
    fn mod_toggle_unknown_file_is_not_found() {
        let f = fixture(true);
        fs::create_dir_all(f.game_dir.join("mods")).unwrap();
        let err = f
            .svc
            .set_mod_enabled(&f.root_id, "1.21.4-fabric", "nope.jar", false)
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn mod_toggle_rejects_bad_extension_and_name() {
        let f = fixture(true);
        fs::create_dir_all(f.game_dir.join("mods")).unwrap();
        let err = f
            .svc
            .set_mod_enabled(&f.root_id, "1.21.4-fabric", "notes.txt", false)
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        let err = f
            .svc
            .set_mod_enabled(&f.root_id, "1.21.4-fabric", "../evil.jar", false)
            .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn packs_read_enabled_state_from_options_txt() {
        let f = fixture(true);
        let packs = f.game_dir.join("resourcepacks");
        touch(&packs.join("Faithful.zip"), b"z");
        touch(&packs.join("VanillaTweaks.zip"), b"z");
        touch(
            &f.game_dir.join("options.txt"),
            "fov:0.5\nresourcePacks:[\"vanilla\",\"file/Faithful.zip\"]\nlang:zh_cn\n".as_bytes(),
        );

        let list = f.svc.resource_packs(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(list.len(), 2);
        let faithful = list.iter().find(|p| p.file_name == "Faithful.zip").unwrap();
        let other = list
            .iter()
            .find(|p| p.file_name == "VanillaTweaks.zip")
            .unwrap();
        assert!(faithful.enabled);
        assert!(!other.enabled);
        assert!(!faithful.missing);
    }

    #[test]
    fn packs_are_never_written_back() {
        let f = fixture(true);
        touch(&f.game_dir.join("resourcepacks/Faithful.zip"), b"z");
        let options = f.game_dir.join("options.txt");
        let original = "fov:0.5\nresourcePacks:[\"vanilla\"]\nlang:zh_cn\n";
        touch(&options, original.as_bytes());

        let _ = f.svc.resource_packs(&f.root_id, "1.21.4-fabric").unwrap();
        let _ = f.svc.saves(&f.root_id, "1.21.4-fabric").unwrap();
        let after = fs::read_to_string(&options).unwrap();
        assert_eq!(after, original, "内容命令不允许改 options.txt");
    }

    #[test]
    fn packs_report_entries_missing_on_disk() {
        let f = fixture(true);
        touch(&f.game_dir.join("resourcepacks/Faithful.zip"), b"z");
        touch(
            &f.game_dir.join("options.txt"),
            "resourcePacks:[\"vanilla\",\"file/Faithful.zip\",\"file/Gone.zip\"]\n".as_bytes(),
        );
        let list = f.svc.resource_packs(&f.root_id, "1.21.4-fabric").unwrap();
        let gone = list.iter().find(|p| p.file_name == "Gone.zip").unwrap();
        assert!(gone.missing);
        assert!(gone.enabled);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn broken_options_txt_is_a_format_error() {
        let f = fixture(true);
        touch(&f.game_dir.join("options.txt"), b"resourcePacks:[oops\n");
        let err = f
            .svc
            .resource_packs(&f.root_id, "1.21.4-fabric")
            .unwrap_err();
        assert!(matches!(err, ApiError::Format(_)), "{err:?}");
    }

    #[test]
    fn saves_parse_level_dat() {
        let f = fixture(true);
        let world = f.game_dir.join("saves/My World");
        fs::create_dir_all(world.join("region")).unwrap();
        fs::write(
            world.join("level.dat"),
            nbt::level_dat_bytes("暮色森林", 1, 1_700_000_000_000, "1.21.4"),
        )
        .unwrap();
        fs::write(world.join("region/r.0.0.mca"), vec![0u8; 64]).unwrap();
        fs::write(world.join("icon.png"), b"png").unwrap();

        let list = f.svc.saves(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(list.len(), 1);
        let save = &list[0];
        assert_eq!(save.folder, "My World");
        assert_eq!(save.name, "暮色森林");
        assert_eq!(save.game_mode, "creative");
        assert_eq!(save.mc_version.as_deref(), Some("1.21.4"));
        // NBT 里是毫秒，出口统一成秒：界面按秒解释才不会算成 5 万年后。
        assert_eq!(save.last_played, Some(1_700_000_000));
        assert!(save.has_icon);
        assert!(save.size_bytes >= 64);
        assert!(!save.size_capped);
    }

    #[test]
    fn saves_last_played_is_seconds_not_millis() {
        let f = fixture(true);
        let world = f.game_dir.join("saves/单位");
        fs::create_dir_all(&world).unwrap();
        fs::write(
            world.join("level.dat"),
            nbt::level_dat_bytes("单位", 0, 1_766_000_000_123, "1.21.4"),
        )
        .unwrap();

        let list = f.svc.saves(&f.root_id, "1.21.4-fabric").unwrap();
        let seconds = list[0].last_played.expect("LastPlayed 应当读出来");
        assert_eq!(seconds, 1_766_000_000);
        // 按秒解释必须落在本世纪；如果按毫秒解释，年份会是 5 万年后。
        assert!(
            (1_500_000_000..2_000_000_000).contains(&seconds),
            "{seconds} 不像 epoch 秒"
        );
    }

    #[test]
    fn saves_skip_dirs_without_level_dat() {
        let f = fixture(true);
        fs::create_dir_all(f.game_dir.join("saves/not-a-world")).unwrap();
        assert!(f.svc.saves(&f.root_id, "1.21.4-fabric").unwrap().is_empty());
    }

    #[test]
    fn saves_with_broken_level_dat_still_listed() {
        let f = fixture(true);
        let world = f.game_dir.join("saves/corrupt");
        fs::create_dir_all(&world).unwrap();
        fs::write(world.join("level.dat"), b"not nbt at all").unwrap();
        let list = f.svc.saves(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "corrupt");
        assert_eq!(list[0].game_mode, "unknown");
    }

    #[test]
    fn logs_list_puts_latest_first() {
        let f = fixture(true);
        let logs = f.game_dir.join("logs");
        touch(&logs.join("2026-10-01-1.log.gz"), b"gz");
        touch(&logs.join("2026-10-02-2.log"), b"log");
        touch(&logs.join("latest.log"), b"latest");
        touch(&logs.join("debug.txt"), b"x");

        let list = f.svc.logs(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].file_name, "latest.log");
        assert!(list[0].is_latest);
        assert!(list
            .iter()
            .any(|l| l.file_name.ends_with(".log.gz") && l.compressed));
    }

    #[test]
    fn logs_read_tail_marks_truncated() {
        let f = fixture(true);
        let logs = f.game_dir.join("logs");
        let mut body = String::new();
        for i in 0..2000 {
            body.push_str(&format!("line {i}\n"));
        }
        touch(&logs.join("latest.log"), body.as_bytes());

        let tail = f
            .svc
            .read_log(&f.root_id, "1.21.4-fabric", "latest.log", 256)
            .unwrap();
        assert!(tail.truncated);
        assert_eq!(tail.shown_bytes, 256);
        assert_eq!(tail.total_bytes, body.len() as u64);
        assert!(tail.text.ends_with("line 1999\n"), "{}", tail.text);
        assert!(!tail.text.contains("line 0\n"));
    }

    #[test]
    fn logs_read_gunzip() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let f = fixture(true);
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(b"hello from old log\n").unwrap();
        let gz = enc.finish().unwrap();
        touch(&f.game_dir.join("logs/2026-10-01-1.log.gz"), &gz);

        let tail = f
            .svc
            .read_log(&f.root_id, "1.21.4-fabric", "2026-10-01-1.log.gz", 4096)
            .unwrap();
        assert_eq!(tail.text, "hello from old log\n");
        assert!(!tail.truncated);
    }

    #[test]
    fn logs_reject_path_traversal() {
        let f = fixture(true);
        fs::create_dir_all(f.game_dir.join("logs")).unwrap();
        for bad in [
            "../options.txt",
            "..\\options.txt",
            "C:\\Windows\\win.ini",
            "a/b.log",
        ] {
            let err = f
                .svc
                .read_log(&f.root_id, "1.21.4-fabric", bad, 64)
                .unwrap_err();
            assert!(matches!(err, ApiError::Invalid(_)), "{bad} → {err:?}");
        }
    }

    #[test]
    fn import_copies_and_skips_existing() {
        let f = fixture(true);
        let src = f._dir.path().join("downloads");
        touch(&src.join("sodium.jar"), b"jar");
        touch(&src.join("sodium.jar.bak"), b"x");
        touch(&src.join("notes.txt"), b"x");
        touch(&f.game_dir.join("mods/sodium.jar"), b"old");

        let outcome = f
            .svc
            .import(
                &f.root_id,
                "1.21.4-fabric",
                ContentKind::Mod,
                &[
                    src.join("sodium.jar"),
                    src.join("other.jar"),
                    src.join("notes.txt"),
                ],
            )
            .unwrap();
        assert!(outcome.copied.is_empty(), "{outcome:?}");
        assert_eq!(outcome.skipped.len(), 3);
        // 同名的不覆盖：内容还是旧的
        assert_eq!(
            fs::read(f.game_dir.join("mods/sodium.jar")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn import_creates_dir_and_copies_new_file() {
        let f = fixture(true);
        let src = f._dir.path().join("dl");
        touch(&src.join("iris-1.8.jar"), b"iris");
        let outcome = f
            .svc
            .import(
                &f.root_id,
                "1.21.4-fabric",
                ContentKind::Mod,
                &[src.join("iris-1.8.jar")],
            )
            .unwrap();
        assert_eq!(outcome.copied, vec!["iris-1.8.jar".to_string()]);
        assert_eq!(
            fs::read(f.game_dir.join("mods/iris-1.8.jar")).unwrap(),
            b"iris"
        );
    }

    #[test]
    fn delete_removes_file_and_reports_missing() {
        let f = fixture(true);
        touch(&f.game_dir.join("mods/sodium.jar"), b"a");
        f.svc
            .delete(&f.root_id, "1.21.4-fabric", ContentKind::Mod, "sodium.jar")
            .unwrap();
        assert!(!f.game_dir.join("mods/sodium.jar").exists());

        let err = f
            .svc
            .delete(&f.root_id, "1.21.4-fabric", ContentKind::Mod, "sodium.jar")
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn delete_removes_pack_directory() {
        let f = fixture(true);
        let pack = f.game_dir.join("resourcepacks/unpacked");
        fs::create_dir_all(&pack).unwrap();
        fs::write(pack.join("pack.mcmeta"), b"{}").unwrap();
        f.svc
            .delete(
                &f.root_id,
                "1.21.4-fabric",
                ContentKind::ResourcePack,
                "unpacked",
            )
            .unwrap();
        assert!(!pack.exists());
    }

    #[test]
    fn split_name_version_heuristics() {
        assert_eq!(
            split_name_version("sodium-fabric-0.6.9"),
            ("sodium-fabric".to_string(), Some("0.6.9".to_string()))
        );
        assert_eq!(
            split_name_version("lithium-0.15.3+mc1.21.4"),
            ("lithium".to_string(), Some("0.15.3+mc1.21.4".to_string()))
        );
        assert_eq!(
            split_name_version("journeymap"),
            ("journeymap".to_string(), None)
        );
        assert_eq!(split_name_version("1.2.3"), ("1.2.3".to_string(), None));
    }

    // -----------------------------------------------------------------------
    // 图标 / 更新检查的纯函数
    // -----------------------------------------------------------------------

    #[test]
    fn icon_mime_only_accepts_images() {
        assert_eq!(icon_mime("assets/mymod/icon.png"), Some("image/png"));
        assert_eq!(icon_mime("icon.PNG"), Some("image/png"));
        assert_eq!(icon_mime("assets/x/icon.jpg"), Some("image/jpeg"));
        assert_eq!(icon_mime("assets/x/icon.jpeg"), Some("image/jpeg"));
        assert_eq!(icon_mime("pack:assets/x/icon.gif"), Some("image/gif"));
        assert_eq!(icon_mime("assets/x/icon.webp"), Some("image/webp"));
        assert_eq!(icon_mime("assets/x/logo.svg"), Some("image/svg+xml"));
        // 目录名里带点不算扩展名，非图片一律拒绝（否则界面会显示破图）
        assert_eq!(icon_mime("assets/mod.icon/icon"), None);
        assert_eq!(icon_mime("assets/x/icon.txt"), None);
        assert_eq!(icon_mime("assets/x/icon"), None);
    }

    #[test]
    fn icon_data_url_is_a_data_uri() {
        let url = icon_data_url("image/png", b"\x89PNG");
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        assert_eq!(url, "data:image/png;base64,iVBORw==");
    }

    #[test]
    fn update_decision_prefers_content_hash() {
        let latest = |version: &str, sha1: Option<&str>| LatestFile {
            source: ModSource::Modrinth,
            project_id: "AANobbMI".into(),
            version: version.to_string(),
            file_name: format!("mod-{version}.jar"),
            url: "https://example.invalid/mod.jar".into(),
            size: 1,
            sha1: sha1.map(str::to_string),
            page_url: None,
        };
        // 哈希相同 = 同一个文件：即使版本号写法不同也不能报"有更新"
        assert!(!decide_update("abc", &latest("9.9.9", Some("abc")), Some("0.6.0")));
        assert!(!decide_update("ABC", &latest("9.9.9", Some("abc")), Some("0.6.0")));
        // 哈希不同、版本号也不同 → 有更新
        assert!(decide_update("abc", &latest("0.6.9", Some("def")), Some("0.6.0")));
        // 版本号相同但文件不同（重新打包）：不报更新，避免无意义的重复下载
        assert!(!decide_update("abc", &latest("0.6.0", Some("def")), Some("0.6.0")));
        // 读不出 jar 内版本时，内容不同就交给用户决定
        assert!(decide_update("abc", &latest("0.6.9", None), None));
    }

    #[test]
    fn unique_file_name_keeps_extension() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_file_name(dir.path(), "sodium.jar"), "sodium.jar");
        touch(&dir.path().join("sodium.jar"), b"a");
        assert_eq!(unique_file_name(dir.path(), "sodium.jar"), "sodium (1).jar");
        // 停用态的扩展名是 `.jar.disabled`，序号要插在 `.jar` 前面
        touch(&dir.path().join("iris.jar.disabled"), b"a");
        assert_eq!(
            unique_file_name(dir.path(), "iris.jar.disabled"),
            "iris (1).jar.disabled"
        );
    }

    /// 手写一个 stored（method 0）zip：本地头 + 内容 + 中央目录 + EOCD。
    ///
    /// 测试里不该联网也不该依赖外部 jar，所以自己拼一个最小可读的包。
    fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        fn u16le(out: &mut Vec<u8>, value: u16) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        fn u32le(out: &mut Vec<u8>, value: u32) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for byte in data {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xedb8_8320 & mask);
                }
            }
            !crc
        }

        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in files {
            let crc = crc32(data);
            let offset = out.len() as u32;
            u32le(&mut out, 0x0403_4b50);
            u16le(&mut out, 20);
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u32le(&mut out, crc);
            u32le(&mut out, data.len() as u32);
            u32le(&mut out, data.len() as u32);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            u32le(&mut central, 0x0201_4b50);
            u16le(&mut central, 20);
            u16le(&mut central, 20);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u32le(&mut central, crc);
            u32le(&mut central, data.len() as u32);
            u32le(&mut central, data.len() as u32);
            u16le(&mut central, name.len() as u16);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u16le(&mut central, 0);
            u32le(&mut central, 0);
            u32le(&mut central, offset);
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        u32le(&mut out, 0x0605_4b50);
        u16le(&mut out, 0);
        u16le(&mut out, 0);
        u16le(&mut out, files.len() as u16);
        u16le(&mut out, files.len() as u16);
        u32le(&mut out, cd_size);
        u32le(&mut out, cd_offset);
        u16le(&mut out, 0);
        out
    }

    const FABRIC_META: &[u8] = br#"{
        "schemaVersion": 1,
        "id": "mymod",
        "name": "My Mod",
        "version": "1.2.3",
        "authors": ["Alice"],
        "icon": "assets/mymod/icon.png"
    }"#;
    const ICON_PNG: &[u8] = b"\x89PNG\r\n\x1a\nICONDATA";

    #[test]
    fn mods_list_reads_jar_metadata_and_reports_icon() {
        let f = fixture(true);
        let jar = stored_zip(&[
            ("fabric.mod.json", FABRIC_META),
            ("assets/mymod/icon.png", ICON_PNG),
        ]);
        touch(&f.game_dir.join("mods/mymod-1.0.0.jar"), &jar);

        let list = f.svc.mods(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(list.len(), 1);
        let entry = &list[0];
        // jar 内真名优先于文件名（文件名里的 1.0.0 是故意写成旧版本的）
        assert_eq!(entry.display_name, "My Mod");
        assert_eq!(entry.version.as_deref(), Some("1.2.3"));
        assert_eq!(entry.mod_id.as_deref(), Some("mymod"));
        assert_eq!(entry.authors, vec!["Alice".to_string()]);
        assert_eq!(entry.meta_source.as_deref(), Some("fabric.mod.json"));
        assert!(entry.has_icon);
        assert!(entry.flavors.contains(&"fabric".to_string()));

        // 图标按需取，返回 data URL
        let icon = f
            .svc
            .mod_icon(&f.root_id, "1.21.4-fabric", "mymod-1.0.0.jar")
            .unwrap();
        assert!(icon.data_url.starts_with("data:image/png;base64,"));
        assert_eq!(icon.mime, "image/png");

        // 元数据缓存：同样的文件再读一次结果一致（缓存没有把字段吃掉）
        let again = f.svc.mods(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(again[0], *entry);
    }

    #[test]
    fn broken_jar_falls_back_to_filename_metadata() {
        let f = fixture(true);
        touch(&f.game_dir.join("mods/sodium-fabric-0.6.9.jar"), b"not a zip");
        let list = f.svc.mods(&f.root_id, "1.21.4-fabric").unwrap();
        assert_eq!(list.len(), 1);
        // 读不出元数据不是错误：回落文件名启发式
        assert_eq!(list[0].meta_source.as_deref(), Some("filename"));
        assert!(!list[0].has_icon);
        assert!(list[0].mod_id.is_none());
        assert_eq!(list[0].version.as_deref(), Some("0.6.9"));

        // 没有图标声明 / 不是图片时是 NotFound，不是 panic 也不是空串
        let err = f
            .svc
            .mod_icon(&f.root_id, "1.21.4-fabric", "sodium-fabric-0.6.9.jar")
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
        let err = f
            .svc
            .mod_icon(&f.root_id, "1.21.4-fabric", "missing.jar")
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn icon_rejects_non_image_and_oversized_entries() {
        let f = fixture(true);
        let jar = stored_zip(&[
            (
                "fabric.mod.json",
                br#"{"schemaVersion":1,"id":"x","name":"X","version":"1","icon":"assets/x/icon.txt"}"#
                    .as_slice(),
            ),
            ("assets/x/icon.txt", b"not an image"),
        ]);
        touch(&f.game_dir.join("mods/x.jar"), &jar);
        let err = f
            .svc
            .mod_icon(&f.root_id, "1.21.4-fabric", "x.jar")
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
        // 列表里仍然标记"有图标"（声明了），取不到是取图那一步的事
        assert!(f.svc.mods(&f.root_id, "1.21.4-fabric").unwrap()[0].has_icon);
    }

    #[test]
    fn icon_rejects_entries_above_the_size_cap() {
        let f = fixture(true);
        // 512 KiB 以上：宁可说"没有图标"，也不把几 MB 的图塞进桥接消息
        let big = vec![0u8; MAX_ICON_BYTES + 1];
        let jar = stored_zip(&[
            (
                "fabric.mod.json",
                br#"{"schemaVersion":1,"id":"x","name":"X","version":"1","icon":"icon.png"}"#.as_slice(),
            ),
            ("icon.png", &big),
        ]);
        touch(&f.game_dir.join("mods/big.jar"), &jar);
        let err = f
            .svc
            .mod_icon(&f.root_id, "1.21.4-fabric", "big.jar")
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }
}
