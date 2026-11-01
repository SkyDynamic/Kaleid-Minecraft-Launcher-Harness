//! 最小只读 ZIP/JAR 读取器 + jar 内模组元数据解析。
//!
//! 为什么要手写：`zip` crate 不在依赖里，而我们真正需要的只有两步——
//! 找到中央目录、解出某个条目的内容（stored / deflate）。为两步引一整套
//! zip 依赖不划算，格式本身（EOCD → 中央目录 → 本地头）也就三段定长记录。
//!
//! 安全边界：jar 是**外部数据**，会坏，也可能有人故意构造。
//! - 条目数、单个条目的解压量、单次调用的累计解压量都有上限；
//! - 解压比超过 200 倍直接拒绝（几十字节解出几 MB 的压缩炸弹）；
//! - 加密条目、zip64 占位值、不认识的压缩方法一律当"读不到"；
//! - 任何越界/错位都返回 `None` / 空列表，绝不 panic。
//!
//! 元数据来源按"信息量从多到少"排序，取第一个能解析出来的：
//! `fabric.mod.json` → `quilt.mod.json` → `neoforge.mods.toml` → `mods.toml`
//! → `MANIFEST.MF`。全都读不出来时返回 `None`，由调用方回落到文件名启发式。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// 尾部记录（End Of Central Directory）。
const SIG_EOCD: u32 = 0x0605_4b50;
/// 中央目录条目。
const SIG_CENTRAL: u32 = 0x0201_4b50;
/// 本地文件头。
const SIG_LOCAL: u32 = 0x0403_4b50;

/// EOCD 的定长部分。
const EOCD_LEN: u64 = 22;
/// EOCD 离文件末尾最远多少字节：22 字节定长 + 最长 65535 字节注释。
const EOCD_WINDOW: u64 = 22 + 65_535;
/// 中央目录里一个条目的定长部分。
const CENTRAL_LEN: usize = 46;

/// 最多认这么多条目：正常模组 jar 也就几百个文件，4096 已经绰绰有余。
/// 注意它只约束"列出条目"这类粗看（`entries`），不约束找元数据（`find`）。
const MAX_ENTRIES: usize = 4096;
/// 一次最多收几份候选元数据：固定就那五份，留点余量。
const META_SLOTS: usize = 8;
/// 中央目录最多读这么多字节：16 MiB 够 ~20 万个条目，防的是超长注释/extra。
const MAX_CENTRAL: u64 = 16 * 1024 * 1024;
/// 单份元数据的解压上限：元数据撑死几十 KB，4 MiB 已很宽容。
const MAX_META_BYTES: u64 = 4 * 1024 * 1024;
/// 单次调用累计解压上限（一次调用可能要试好几份候选元数据）。
const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
/// 压缩体本身的读入上限：比解压上限略松，deflate 对不可压数据会有少量膨胀。
const MAX_COMPRESSED: u64 = MAX_META_BYTES + 64 * 1024;
/// 解压比上限（解压后 / 压缩后）。
const MAX_RATIO: u64 = 200;

const FABRIC_META: &str = "fabric.mod.json";
const QUILT_META: &str = "quilt.mod.json";
const NEOFORGE_META: &str = "META-INF/neoforge.mods.toml";
const FORGE_META: &str = "META-INF/mods.toml";
const MANIFEST_META: &str = "META-INF/MANIFEST.MF";

/// jar 内声明的模组信息。
///
/// 字段全是 `Option`：真实 jar 里什么都能缺（有 mod 只写了 `modId`），
/// 缺哪个就让调用方去补哪个，而不是在这里编一个默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModMeta {
    pub mod_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub authors: Vec<String>,
    /// 声明支持的加载器：`fabric` / `quilt` / `forge` / `neoforge`
    pub flavors: Vec<String>,
    /// jar 内图标的条目名（如 `assets/mymod/icon.png`）
    pub icon_entry: Option<String>,
    /// 元数据来源：`fabric.mod.json` / `quilt.mod.json` / `neoforge.mods.toml` / `mods.toml` / `manifest` / `filename`
    pub source: String,
}

/// 读取 jar 内的模组元数据；不支持/损坏/超限时返回 None。
pub fn read_mod_meta(path: &Path) -> Option<ModMeta> {
    let mut jar = Jar::open(path)?;
    // 只挑这五份候选：走完整条中央目录，不漏排在大 jar 末尾的元数据。
    let entries = jar.find(is_meta_entry, META_SLOTS)?;
    let mut spent = 0u64;

    // 顺序即优先级：先看信息最全的 JSON，再看 TOML，最后看只有名字的 MANIFEST。
    if let Some(bytes) = probe(&mut jar, &entries, FABRIC_META, &mut spent) {
        if let Some(meta) = parse_fabric(&bytes) {
            return Some(meta);
        }
    }
    if let Some(bytes) = probe(&mut jar, &entries, QUILT_META, &mut spent) {
        if let Some(meta) = parse_quilt(&bytes) {
            return Some(meta);
        }
    }
    if let Some(bytes) = probe(&mut jar, &entries, NEOFORGE_META, &mut spent) {
        if let Some(meta) = parse_toml_meta(&bytes, "neoforge.mods.toml", "neoforge") {
            return Some(meta);
        }
    }
    if let Some(bytes) = probe(&mut jar, &entries, FORGE_META, &mut spent) {
        if let Some(meta) = parse_toml_meta(&bytes, "mods.toml", "forge") {
            return Some(meta);
        }
    }
    if let Some(bytes) = probe(&mut jar, &entries, MANIFEST_META, &mut spent) {
        if let Some(meta) = parse_manifest(&bytes) {
            return Some(meta);
        }
    }
    None
}

/// 元数据只可能出现在这几个固定路径上。
fn is_meta_entry(name: &str) -> bool {
    name == FABRIC_META
        || name == QUILT_META
        || name == NEOFORGE_META
        || name == FORGE_META
        || name == MANIFEST_META
}

/// 读取 jar 内某个条目的原始字节（解压后）。
///
/// `cap` 是上限而不是"截断长度"：声明体积超过上限就直接返回 `None`，
/// 因为半截的 png / json 比"没有"更糟——上层会当成真数据继续用。
/// 除 `cap` 外还有 4 MiB 的硬顶，避免调用方传一个大数字把闸门开到底。
pub fn read_entry_bytes(path: &Path, entry: &str, cap: usize) -> Option<Vec<u8>> {
    let mut jar = Jar::open(path)?;
    // 同样走完整条目录：图标、mixins 配置在大 jar 里也排得很靠后。
    let entries = jar.find(|name| name == entry, 1)?;
    let mut spent = 0u64;
    let target = entries.first()?;
    jar.load(target, cap as u64, &mut spent)
}

/// 列出 jar 内全部条目名（用于判断是否含 mixin 配置等），带上限。
pub fn list_entries(path: &Path, cap: usize) -> Vec<String> {
    let Some(mut jar) = Jar::open(path) else {
        return Vec::new();
    };
    let Some(entries) = jar.entries() else {
        return Vec::new();
    };
    entries
        .into_iter()
        .take(cap)
        .map(|entry| entry.name)
        .collect()
}

/// 取一份候选元数据：条目不存在、解压失败、超限都只是"这一份没有"。
fn probe(jar: &mut Jar, entries: &[Entry], name: &str, spent: &mut u64) -> Option<Vec<u8>> {
    let entry = entries.iter().find(|entry| entry.name == name)?;
    jar.load(entry, MAX_META_BYTES, spent)
}

/// 中央目录里的一个条目。
struct Entry {
    name: String,
    /// 通用标志位（bit 0 = 加密）。
    flags: u16,
    method: u16,
    compressed: u64,
    uncompressed: u64,
    local_offset: u64,
}

/// 打开的文件 + 长度。长度缓存下来，省得每次判断越界都去问一次文件系统。
struct Jar {
    file: File,
    len: u64,
}

impl Jar {
    fn open(path: &Path) -> Option<Jar> {
        let file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        Some(Jar { file, len })
    }

    /// 读出中央目录里的全部条目（最多 `MAX_ENTRIES` 个）；结构不合法就返回 `None`。
    ///
    /// 这适合"随便看看 jar 里有什么"。**找元数据不能用它**——条目多的 jar 里
    /// `fabric.mod.json` 常常排在最后，数到上限就停会直接看漏，改用 `find`。
    fn entries(&mut self) -> Option<Vec<Entry>> {
        self.walk(|_| true, MAX_ENTRIES)
    }

    /// 按名字挑条目：**整条中央目录都会走完**，只留下 `want` 认的那些。
    ///
    /// 上限施加在"留下了多少"上，不是在"看过多少"上。差别在真实 jar 上很致命：
    /// decocraft-3.0.7-1.21.1-fabric.jar 有 21611 个条目，`META-INF/mods.toml`
    /// （一份没展开的 Gradle 模板）排在最前、`fabric.mod.json`（真身）排在最后，
    /// 按数量截断就只会读到模板，界面上于是显示 `${mod_name}` / `${mod_version}`。
    fn find(&mut self, want: impl Fn(&str) -> bool, max_kept: usize) -> Option<Vec<Entry>> {
        self.walk(want, max_kept)
    }

    /// 走中央目录。`want` 决定留下哪些，`max_kept` 是留下多少就收工。
    fn walk(&mut self, want: impl Fn(&str) -> bool, max_kept: usize) -> Option<Vec<Entry>> {
        if self.len < EOCD_LEN {
            return None;
        }

        // EOCD 在文件末尾最多 64 KiB 内。注释里可能恰好出现同样的字节，
        // 所以从后往前找，取最靠后的那个签名才符合规范。
        let window = self.len.min(EOCD_WINDOW);
        let mut tail = vec![0u8; window as usize];
        self.file.seek(SeekFrom::Start(self.len - window)).ok()?;
        self.file.read_exact(&mut tail).ok()?;

        let mut at = tail.len() - EOCD_LEN as usize;
        let eocd = loop {
            if u32_at(&tail, at) == Some(SIG_EOCD) {
                break at;
            }
            if at == 0 {
                return None;
            }
            at -= 1;
        };

        let total = u64::from(u16_at(&tail, eocd + 10)?);
        let cd_size = u64::from(u32_at(&tail, eocd + 12)?);
        let cd_offset = u64::from(u32_at(&tail, eocd + 16)?);
        // zip64 用 0xFFFF / 0xFFFFFFFF 占位，真实值在额外的 zip64 记录里。
        // 模组 jar 不会这么大，直接当不支持。
        if total == u64::from(u16::MAX)
            || cd_size == u64::from(u32::MAX)
            || cd_offset == u64::from(u32::MAX)
        {
            return None;
        }
        if cd_offset >= self.len {
            return None;
        }

        let cd_len = cd_size.min(self.len - cd_offset).min(MAX_CENTRAL);
        if cd_len < CENTRAL_LEN as u64 {
            return None;
        }
        let mut cd = vec![0u8; cd_len as usize];
        self.file.seek(SeekFrom::Start(cd_offset)).ok()?;
        self.file.read_exact(&mut cd).ok()?;

        let mut out = Vec::new();
        let mut pos = 0usize;
        let mut seen = 0u64;
        while seen < total && out.len() < max_kept {
            let Some(sig) = u32_at(&cd, pos) else { break };
            if sig != SIG_CENTRAL {
                break;
            }
            // 定长部分的字段全都落在 pos..pos+46 内，取不到只可能是记录被截断。
            let Some(flags) = u16_at(&cd, pos + 8) else {
                break;
            };
            let Some(method) = u16_at(&cd, pos + 10) else {
                break;
            };
            let Some(compressed) = u32_at(&cd, pos + 20) else {
                break;
            };
            let Some(uncompressed) = u32_at(&cd, pos + 24) else {
                break;
            };
            let Some(name_len) = u16_at(&cd, pos + 28) else {
                break;
            };
            let Some(extra_len) = u16_at(&cd, pos + 30) else {
                break;
            };
            let Some(comment_len) = u16_at(&cd, pos + 32) else {
                break;
            };
            let Some(local_offset) = u32_at(&cd, pos + 42) else {
                break;
            };

            let Some(name_start) = pos.checked_add(CENTRAL_LEN) else {
                break;
            };
            let Some(name_end) = name_start.checked_add(usize::from(name_len)) else {
                break;
            };
            let Some(raw_name) = cd.get(name_start..name_end) else {
                break;
            };
            let name = String::from_utf8_lossy(raw_name).into_owned();
            if want(&name) {
                out.push(Entry {
                    name,
                    flags,
                    method,
                    compressed: u64::from(compressed),
                    uncompressed: u64::from(uncompressed),
                    local_offset: u64::from(local_offset),
                });
            }

            let step = CENTRAL_LEN
                + usize::from(name_len)
                + usize::from(extra_len)
                + usize::from(comment_len);
            let Some(next) = pos.checked_add(step) else {
                break;
            };
            pos = next;
            seen += 1;
        }
        Some(out)
    }

    /// 取一个条目的内容。`spent` 是调用方的累计预算，跨多次读取共享。
    fn load(&mut self, entry: &Entry, cap: u64, spent: &mut u64) -> Option<Vec<u8>> {
        // 加密条目（标志位 bit 0）：没有密码，不装作能读。
        if entry.flags & 0x0001 != 0 {
            return None;
        }
        if entry.method != 0 && entry.method != 8 {
            return None;
        }
        // 大小字段是 zip64 占位值：真实大小在 extra 里，我们不解析。
        if entry.compressed == u64::from(u32::MAX) || entry.uncompressed == u64::from(u32::MAX) {
            return None;
        }

        let limit = cap.min(MAX_META_BYTES);
        // 声明体积超过上限就不试了：不为了一个坏 jar 先申请一大块内存。
        if entry.uncompressed > limit {
            return None;
        }
        // 压缩炸弹：几十字节解出几 MB 是经典手法。
        if entry.compressed > 0 && entry.uncompressed > entry.compressed.saturating_mul(MAX_RATIO) {
            return None;
        }
        if entry.compressed > MAX_COMPRESSED {
            return None;
        }
        // 单次调用的预算：read_mod_meta 会连着试几份候选，总量也得有闸。
        *spent = spent.checked_add(entry.uncompressed)?;
        if *spent > MAX_TOTAL_BYTES {
            return None;
        }

        // 数据起点只能按**本地头**算：规范允许本地头的 extra 与中央目录不同，
        // 拿中央目录的 extra 长度去偏移会读到错位的字节。
        let mut head = [0u8; 30];
        self.file.seek(SeekFrom::Start(entry.local_offset)).ok()?;
        self.file.read_exact(&mut head).ok()?;
        if u32_at(&head, 0) != Some(SIG_LOCAL) {
            return None;
        }
        let name_len = u64::from(u16_at(&head, 26)?);
        let extra_len = u64::from(u16_at(&head, 28)?);
        let data_start = entry
            .local_offset
            .checked_add(30)?
            .checked_add(name_len)?
            .checked_add(extra_len)?;

        // 长度为 0 的条目（目录、空文件）没有数据段。
        if entry.compressed == 0 && entry.uncompressed == 0 {
            return Some(Vec::new());
        }
        // 压缩长度为 0 但内容非空：流式写入（data descriptor）的 jar 有这种，
        // 按定长读会读错，宁可不读。
        if entry.compressed == 0 {
            return None;
        }
        if data_start.checked_add(entry.compressed)? > self.len {
            return None;
        }

        let mut body = vec![0u8; entry.compressed as usize];
        self.file.seek(SeekFrom::Start(data_start)).ok()?;
        self.file.read_exact(&mut body).ok()?;

        match entry.method {
            // stored：body 就是内容本身
            0 => Some(body),
            // deflate：头里的长度可能是假的，解压过程也按 limit 截一道
            _ => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(body.as_slice())
                    .take(limit)
                    .read_to_end(&mut out)
                    .ok()?;
                Some(out)
            }
        }
    }
}

fn u16_at(buf: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    let raw = buf.get(at..end)?;
    Some(u16::from_le_bytes([raw[0], raw[1]]))
}

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let raw = buf.get(at..end)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// 去掉首尾空白；空串等于没有。
/// Gradle / Maven 的模板占位符：`${mod_name}`、`@VERSION@`、`%mod_name%`。
///
/// 有些 mod 会把**没展开的模板**一起打进 jar（decocraft 就同时带了展开好的
/// `fabric.mod.json` 和一份模板 `mods.toml`）。把它当真实名字显示出来，
/// 用户看到的就是「模组名 ${mod_name}」，比"读不出来、退回文件名"糟得多。
fn is_placeholder(text: &str) -> bool {
    let text = text.trim();
    if text.contains("${") {
        return true;
    }
    let quoted = |delim: char| text.len() > 2 && text.starts_with(delim) && text.ends_with(delim);
    quoted('@') || quoted('%')
}

fn non_empty(raw: &str) -> Option<String> {
    let text = raw.trim();
    if text.is_empty() || is_placeholder(text) {
        None
    } else {
        Some(text.to_string())
    }
}

fn text_of(value: Option<&Value>) -> Option<String> {
    non_empty(value?.as_str()?)
}

/// `${file.jarVersion}` 是 Forge/NeoForge 的"版本号由文件名补"占位符，
/// 原样显示只会让人以为模组版本叫这个名字。其余占位符同理，一并当读不出来。
fn clean_version(raw: &str) -> Option<String> {
    non_empty(raw)
}

fn version_of(value: Option<&Value>) -> Option<String> {
    clean_version(value?.as_str()?)
}

/// fabric 的 `authors`：字符串数组，元素也可能是 `{"name": "..."}` 对象。
fn authors_of(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(name) => non_empty(name),
                other => text_of(other.get("name")),
            })
            .collect(),
        // 少数 mod 直接写一个逗号分隔的字符串，按它的本意拆开
        Some(Value::String(list)) => list.split(',').filter_map(non_empty).collect(),
        _ => Vec::new(),
    }
}

/// 图标只认 jar 内路径：网络地址和 data URI 都取不出来，等于没有。
fn icon_of(value: Option<&Value>) -> Option<String> {
    let raw = match value? {
        Value::String(path) => path.clone(),
        // fabric 允许按分辨率给多份（`{"64": "assets/..."}`），取第一个能用的
        Value::Object(map) => map.values().find_map(|value| value.as_str())?.to_string(),
        _ => return None,
    };
    let path = raw.trim();
    if path.is_empty()
        || path.starts_with("http://")
        || path.starts_with("https://")
        || path.starts_with("data:")
    {
        return None;
    }
    Some(path.to_string())
}

fn parse_fabric(bytes: &[u8]) -> Option<ModMeta> {
    let json: Value = serde_json::from_slice(bytes).ok()?;
    let mod_id = text_of(json.get("id"));
    let name = text_of(json.get("name")).or_else(|| text_of(json.get("title")));
    let version = version_of(json.get("version"));
    // id、名字、版本一个都拿不到（模板没展开就是这种形状），
    // 说明这不是我们能用的 fabric 元数据，让下一种格式再试。
    if mod_id.is_none() && name.is_none() && version.is_none() {
        return None;
    }
    Some(ModMeta {
        mod_id,
        name,
        version,
        authors: authors_of(json.get("authors")),
        flavors: vec!["fabric".to_string()],
        icon_entry: icon_of(json.get("icon")),
        source: FABRIC_META.to_string(),
    })
}

fn parse_quilt(bytes: &[u8]) -> Option<ModMeta> {
    let json: Value = serde_json::from_slice(bytes).ok()?;
    // quilt 把一切都放在 `quilt_loader` 下，没有它就是别的格式。
    let loader = json.get("quilt_loader")?;
    let meta = loader.get("metadata");
    let mod_id = text_of(loader.get("id"));
    let name = meta.and_then(|meta| text_of(meta.get("name")));
    if mod_id.is_none() && name.is_none() {
        return None;
    }
    Some(ModMeta {
        mod_id,
        name,
        version: version_of(loader.get("version")),
        // quilt 的名单在 `metadata.contributors` 里且是"作者 → 角色"的映射，
        // 语义和 fabric 的 authors 不完全一样，宁可不猜。
        authors: Vec::new(),
        flavors: vec!["quilt".to_string()],
        icon_entry: meta.and_then(|meta| icon_of(meta.get("icon"))),
        source: QUILT_META.to_string(),
    })
}

/// `mods.toml` 的解析结果（只收我们关心的几个键）。
#[derive(Default)]
struct TomlMods {
    mod_id: Option<String>,
    display_name: Option<String>,
    version: Option<String>,
    logo_file: Option<String>,
    authors: Vec<String>,
}

/// 一个极小的 TOML 子集解析器。
///
/// 为什么要手写：`mods.toml` 里 99% 的内容是 `[[dependencies.x]]` 之类的嵌套表，
/// 我们只要第一个 `[[mods]]` 块里的四个键。为这点需求引一个 TOML 解析器
/// （以及它的 serde 约定）不值得，而且真正的 TOML 语法我们永远不会用到。
fn parse_toml_meta(bytes: &[u8], source: &str, flavor: &str) -> Option<ModMeta> {
    let text = String::from_utf8_lossy(bytes);
    let mut found = TomlMods::default();
    let mut in_mods = false;
    let mut mods_seen = false;

    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if line == "[[mods]]" {
                // 只认第一个模组块，后面的块是 jar 里的其它模组（shadow 依赖）。
                in_mods = !mods_seen;
                mods_seen = true;
            } else if in_mods {
                // 离开模组块（`[[dependencies.x]]` 等），之后的键不属于它。
                in_mods = false;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let parsed = toml_value(value);
        // `authors` 在 mods.toml 里是顶层键，所以放在块判断之前收。
        if key == "authors" {
            if let TomlValue::Text(list) = parsed {
                found.authors = list.split(',').filter_map(non_empty).collect();
            }
            continue;
        }
        if !in_mods {
            continue;
        }
        match key {
            "modId" => found.mod_id = text_value(&parsed),
            "displayName" => found.display_name = text_value(&parsed),
            "version" => found.version = text_value(&parsed).and_then(|v| clean_version(&v)),
            "logoFile" => found.logo_file = text_value(&parsed),
            _ => {}
        }
    }

    let mod_id = found.mod_id;
    let name = found.display_name;
    // 三样全空（例如整份文件都是没展开的模板）就当这份 TOML 读不出来，
    // 让后面的 MANIFEST / 文件名启发式接手，别拿占位符当真值。
    if mod_id.is_none() && name.is_none() && found.version.is_none() {
        return None;
    }
    Some(ModMeta {
        mod_id,
        name,
        version: found.version,
        authors: found.authors,
        flavors: vec![flavor.to_string()],
        // `logoFile` 本来就是 jar 内路径
        icon_entry: found.logo_file,
        source: source.to_string(),
    })
}

/// `MANIFEST.MF` 只兜底：没有 modId，只有打包工具写进去的名字和版本。
fn parse_manifest(bytes: &[u8]) -> Option<ModMeta> {
    let text = String::from_utf8_lossy(bytes);
    // 先收成 (小写键, 值)：manifest 的键大小写不统一，值还有 72 字节折行。
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(' ') {
            // 行首一个空格 = 上一行值的续行
            if let Some(last) = headers.last_mut() {
                last.1.push_str(rest);
            }
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let get = |key: &str| {
        headers
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, v)| v.as_str())
    };

    let name = get("implementation-title").and_then(non_empty);
    let version = get("implementation-version").and_then(clean_version);
    if name.is_none() && version.is_none() {
        return None;
    }
    Some(ModMeta {
        name,
        version,
        source: "manifest".to_string(),
        ..ModMeta::default()
    })
}

/// TOML 值的极小子集。
///
/// `true` / `false`、数组、内联表都归到 `Ignored`：我们读的四个键在真实文件里
/// 从来不是这些形状，为"忽略"单独造一个布尔分支没有意义。
enum TomlValue {
    /// 字符串与裸数字归一成文本：`version = 1` 也得能显示成 `1`。
    Text(String),
    Ignored,
}

fn toml_value(raw: &str) -> TomlValue {
    let raw = raw.trim();
    if let Some(text) = unquote(raw) {
        return TomlValue::Text(text);
    }
    // 裸数字（`version = 1`、`1.0`）：只收这种一眼能当文本用的形状
    if !raw.is_empty()
        && raw
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '+' | '-' | '_'))
    {
        return TomlValue::Text(raw.replace('_', ""));
    }
    TomlValue::Ignored
}

fn text_value(value: &TomlValue) -> Option<String> {
    match value {
        TomlValue::Text(text) => non_empty(text),
        TomlValue::Ignored => None,
    }
}

/// 把带引号的值脱成裸文本。
///
/// 三种情况都要认：`"..."`、`'...'`、`"""..."""` / `'''...'''`。
/// 多行字符串里可能有一堆 `#` 和 `=`，脱不掉就会把后面的行带歪。
fn unquote(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    if raw.len() >= 6 && (raw.starts_with("\"\"\"") || raw.starts_with("'''")) {
        return raw.get(3..raw.len() - 3).map(|inner| inner.to_string());
    }
    if bytes[bytes.len() - 1] != quote {
        return None;
    }
    let inner = raw.get(1..raw.len() - 1)?;
    Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

/// 去掉行尾注释。引号内的 `#` 不是注释（许可证 URL、描述文本里常见）。
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (at, ch) in line.char_indices() {
        match ch {
            '"' | '\'' if quote == Some(ch) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            '#' if quote.is_none() => return &line[..at],
            _ => {}
        }
    }
    line
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// 手写 CRC32（与 zip 规范一致的反射多项式 0xEDB88320）。
    /// 没有 zip 写库，fixture 只能自己拼字节，CRC 也就得自己算。
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

    /// 拼一个合法的 zip：本地头 + 内容 + 中央目录 + EOCD。
    /// `pack` 负责把内容压成条目体，`method` 原样写进头部。
    fn assemble(files: &[(&str, &[u8])], method: u16, pack: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
        fn u16le(out: &mut Vec<u8>, value: u16) {
            out.extend_from_slice(&value.to_le_bytes());
        }
        fn u32le(out: &mut Vec<u8>, value: u32) {
            out.extend_from_slice(&value.to_le_bytes());
        }

        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data) in files {
            let body = pack(data);
            let crc = crc32(data);
            let offset = out.len() as u32;

            // 本地文件头，30 字节
            u32le(&mut out, SIG_LOCAL);
            u16le(&mut out, 20); // 解压所需版本
            u16le(&mut out, 0); // 标志位
            u16le(&mut out, method);
            u16le(&mut out, 0); // 修改时间
            u16le(&mut out, 0); // 修改日期
            u32le(&mut out, crc);
            u32le(&mut out, body.len() as u32);
            u32le(&mut out, data.len() as u32);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0); // extra 长度
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&body);

            // 中央目录项，46 字节
            u32le(&mut central, SIG_CENTRAL);
            u16le(&mut central, 20); // 生成版本
            u16le(&mut central, 20); // 解压所需版本
            u16le(&mut central, 0); // 标志位
            u16le(&mut central, method);
            u16le(&mut central, 0); // 修改时间
            u16le(&mut central, 0); // 修改日期
            u32le(&mut central, crc);
            u32le(&mut central, body.len() as u32);
            u32le(&mut central, data.len() as u32);
            u16le(&mut central, name.len() as u16);
            u16le(&mut central, 0); // extra 长度
            u16le(&mut central, 0); // 注释长度
            u16le(&mut central, 0); // 起始磁盘号
            u16le(&mut central, 0); // 内部属性
            u32le(&mut central, 0); // 外部属性
            u32le(&mut central, offset); // 本地头偏移
            central.extend_from_slice(name.as_bytes());
        }

        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);

        // EOCD，22 字节
        u32le(&mut out, SIG_EOCD);
        u16le(&mut out, 0); // 磁盘号
        u16le(&mut out, 0); // 中央目录所在磁盘号
        u16le(&mut out, files.len() as u16);
        u16le(&mut out, files.len() as u16);
        u32le(&mut out, cd_size);
        u32le(&mut out, cd_offset);
        u16le(&mut out, 0); // 注释长度
        out
    }

    /// stored（method 0）的 zip。`pub(crate)`：别的模块（natives 解压）也要造 jar。
    pub(crate) fn zip_stored(files: &[(&str, &[u8])]) -> Vec<u8> {
        assemble(files, 0, |data| data.to_vec())
    }

    /// 真实 jar 基本都是 deflate 的，只测 stored 等于只测了一半路径。
    fn zip_deflated(files: &[(&str, &[u8])]) -> Vec<u8> {
        assemble(files, 8, |data| {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(data).expect("写内存不会失败");
            encoder.finish().expect("写内存不会失败")
        })
    }

    pub(crate) fn write_jar(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("临时目录应当可写");
        path
    }

    const FABRIC_JSON: &[u8] = br#"{
        "schemaVersion": 1,
        "id": "mymod",
        "name": "My Mod",
        "version": "1.2.3",
        "authors": ["Alice", { "name": "Bob" }],
        "icon": { "64": "assets/mymod/icon.png" }
    }"#;

    const ICON: &[u8] = b"\x89PNG\r\n\x1a\nICON";

    #[test]
    fn parses_fabric_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(
            dir.path(),
            "mymod.jar",
            &zip_stored(&[(FABRIC_META, FABRIC_JSON), ("assets/mymod/icon.png", ICON)]),
        );

        let meta = read_mod_meta(&path).expect("fabric 元数据应当能读出来");
        assert_eq!(meta.mod_id.as_deref(), Some("mymod"));
        assert_eq!(meta.name.as_deref(), Some("My Mod"));
        assert_eq!(meta.version.as_deref(), Some("1.2.3"));
        assert_eq!(meta.authors, vec!["Alice".to_string(), "Bob".to_string()]);
        assert_eq!(meta.icon_entry.as_deref(), Some("assets/mymod/icon.png"));
        assert_eq!(meta.flavors, vec!["fabric".to_string()]);
        assert_eq!(meta.source, "fabric.mod.json");
    }

    #[test]
    fn parses_deflated_jar() {
        // 覆盖 method 8：解压、本地头偏移、以及从 deflate 条目取图标字节。
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(
            dir.path(),
            "deflated.jar",
            &zip_deflated(&[(FABRIC_META, FABRIC_JSON), ("assets/mymod/icon.png", ICON)]),
        );

        let meta = read_mod_meta(&path).expect("deflate 条目也要能读");
        assert_eq!(meta.mod_id.as_deref(), Some("mymod"));
        assert_eq!(
            read_entry_bytes(&path, "assets/mymod/icon.png", 4096).as_deref(),
            Some(ICON)
        );
    }

    #[test]
    fn parses_neoforge_toml() {
        // 真实 neoforge 文件的样子：顶层若干键、一个 [[mods]]、后面还有依赖表。
        // 这里用 `r#"..."#` 而不是字节串字面量：文件里有中文，字节串只允许 ASCII。
        let toml = r#"modLoader="javafml"
loaderVersion="[21,)"
license="MIT"
authors="Neo Dev, Second Dev"
# 注释行不该影响解析，URL 里的 # 也不算注释：https://example.com/#frag
[[mods]]
modId="neomymod"
displayName="Neo Mod"
version="${file.jarVersion}"
logoFile="neomod.png"
description='''多行说明，里面 # 不是注释
也应当被忽略'''
[[dependencies.neomymod]]
modId="neoforge"
"#
        .as_bytes();
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(dir.path(), "neo.jar", &zip_stored(&[(NEOFORGE_META, toml)]));

        let meta = read_mod_meta(&path).expect("neoforge 元数据应当能读出来");
        assert_eq!(meta.mod_id.as_deref(), Some("neomymod"));
        assert_eq!(meta.name.as_deref(), Some("Neo Mod"));
        // 占位符等于没有版本号
        assert_eq!(meta.version, None);
        assert_eq!(meta.icon_entry.as_deref(), Some("neomod.png"));
        assert_eq!(
            meta.authors,
            vec!["Neo Dev".to_string(), "Second Dev".to_string()]
        );
        assert_eq!(meta.flavors, vec!["neoforge".to_string()]);
        assert_eq!(meta.source, "neoforge.mods.toml");
    }

    #[test]
    fn parses_forge_toml_with_numeric_version() {
        // `mods.toml` 是 1.12 时代的写法，版本号有可能是裸数字。
        let toml = b"modLoader=\"javafml\"\n[[mods]]\nmodId=\"oldmod\"\ndisplayName=\"Old Mod\"\nversion=1\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(dir.path(), "old.jar", &zip_stored(&[(FORGE_META, toml)]));

        let meta = read_mod_meta(&path).expect("forge 元数据应当能读出来");
        assert_eq!(meta.mod_id.as_deref(), Some("oldmod"));
        assert_eq!(meta.version.as_deref(), Some("1"));
        assert_eq!(meta.flavors, vec!["forge".to_string()]);
        assert_eq!(meta.source, "mods.toml");
    }

    #[test]
    fn falls_back_to_manifest() {
        let manifest = b"Manifest-Version: 1.0\r\nImplementation-Title: Legacy Mod\r\nImplementation-Version: 2.0.1\r\n\r\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(
            dir.path(),
            "legacy.jar",
            &zip_stored(&[(MANIFEST_META, manifest)]),
        );

        let meta = read_mod_meta(&path).expect("manifest 兜底应当能用");
        assert_eq!(meta.name.as_deref(), Some("Legacy Mod"));
        assert_eq!(meta.version.as_deref(), Some("2.0.1"));
        assert_eq!(meta.mod_id, None);
        assert!(meta.flavors.is_empty());
        assert_eq!(meta.source, "manifest");
    }

    #[test]
    fn jar_without_metadata_has_no_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(
            dir.path(),
            "plain.jar",
            &zip_stored(&[
                ("assets/plain/data.txt", b"hi"),
                ("mymod.mixins.json", b"{}"),
            ]),
        );

        // 没有元数据 → None，交给调用方的文件名启发式
        assert!(read_mod_meta(&path).is_none());
        // 但条目还是列得出来（判断有没有 mixin 配置就靠它）
        assert_eq!(
            list_entries(&path, 16),
            vec![
                "assets/plain/data.txt".to_string(),
                "mymod.mixins.json".to_string()
            ]
        );
    }

    #[test]
    fn read_entry_bytes_respects_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(
            dir.path(),
            "icon.jar",
            &zip_stored(&[(FABRIC_META, FABRIC_JSON), ("assets/mymod/icon.png", ICON)]),
        );

        assert_eq!(
            read_entry_bytes(&path, "assets/mymod/icon.png", 4096).as_deref(),
            Some(ICON)
        );
        // cap 比条目小：不给半截数据，直接当"读不到"
        assert!(read_entry_bytes(&path, "assets/mymod/icon.png", 4).is_none());
        // 条目不存在
        assert!(read_entry_bytes(&path, "assets/mymod/none.png", 4096).is_none());
    }

    #[test]
    fn rejects_corrupt_or_truncated_jar() {
        let dir = tempfile::tempdir().unwrap();
        let good = zip_stored(&[(FABRIC_META, FABRIC_JSON)]);

        // 截掉一半：EOCD 和中央目录都没了
        let half = write_jar(dir.path(), "half.jar", &good[..good.len() / 2]);
        assert!(read_mod_meta(&half).is_none());
        assert!(list_entries(&half, 16).is_empty());

        // EOCD 签名被改坏
        let mut broken = good.clone();
        let last = broken.len() - 22;
        broken[last] ^= 0xff;
        let broken = write_jar(dir.path(), "broken.jar", &broken);
        assert!(read_mod_meta(&broken).is_none());

        // 空文件 / 根本不是 zip
        let empty = write_jar(dir.path(), "empty.jar", b"");
        assert!(read_mod_meta(&empty).is_none());
        assert!(list_entries(&empty, 16).is_empty());
        let noise = write_jar(dir.path(), "noise.jar", b"not a zip, just text");
        assert!(read_mod_meta(&noise).is_none());
    }

    #[test]
    fn rejects_entry_that_lies_about_its_size() {
        // 几十字节的 deflate 体，中央目录里却声称解压后 3 MiB：压缩炸弹的经典形状。
        let mut blob = zip_deflated(&[(FABRIC_META, FABRIC_JSON)]);
        let eocd = blob.len() - 22;
        let cd_offset = u32_at(&blob, eocd + 16).expect("EOCD 里应有中央目录偏移") as usize;
        assert_eq!(u32_at(&blob, cd_offset), Some(SIG_CENTRAL));
        blob[cd_offset + 24..cd_offset + 28].copy_from_slice(&(3 * 1024 * 1024u32).to_le_bytes());

        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(dir.path(), "bomb.jar", &blob);
        assert!(
            read_mod_meta(&path).is_none(),
            "解压比超过 200 倍应当直接拒绝"
        );
    }

    /// `META-INF/mods.toml` 里的 Gradle 占位符原样出现在界面上，就说明我们
    /// 读了不该读的那份文件。真实案例：decocraft-3.0.7-1.21.1-fabric.jar。
    const TEMPLATE_TOML: &[u8] = br#"modLoader="javafml"
loaderVersion="${loader_version_range}"
license="${mod_license}"
[[mods]]
modId="${mod_id}"
version="${mod_version}"
displayName="${mod_name}"
description='''${mod_description}'''
"#;

    #[test]
    fn metadata_at_the_end_of_a_huge_jar_is_still_found() {
        // 21k 个条目的大 jar：`fabric.mod.json` 在中央目录的最后，
        // 而 `META-INF/mods.toml`（未展开的模板）在最前面。
        // 读元数据不能因为"条目太多"就看漏后面那份真身。
        let dir = tempfile::tempdir().unwrap();
        let filler: Vec<String> = (0..4_200)
            .map(|index| format!("data/loot_table/blocks/block_{index}.json"))
            .collect();
        let mut files: Vec<(&str, &[u8])> = vec![(FORGE_META, TEMPLATE_TOML)];
        files.extend(filler.iter().map(|name| (name.as_str(), b"{}".as_slice())));
        files.push((FABRIC_META, FABRIC_JSON));
        files.push(("assets/mymod/icon.png", ICON));

        let path = write_jar(dir.path(), "huge.jar", &zip_stored(&files));
        let meta = read_mod_meta(&path).expect("真实元数据应当能读出来");
        assert_eq!(meta.source, "fabric.mod.json");
        assert_eq!(meta.name.as_deref(), Some("My Mod"));
        assert_eq!(meta.version.as_deref(), Some("1.2.3"));
        // 图标也在末尾：按条目名取内容同样不能受条目总数影响
        assert_eq!(
            read_entry_bytes(&path, "assets/mymod/icon.png", 4096).as_deref(),
            Some(ICON)
        );
    }

    #[test]
    fn template_placeholders_are_not_metadata() {
        // 只剩模板时不能把它当元数据：`${mod_name}` 比"读不出来"更糟，
        // 上层会拿它当真实名字显示。
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(dir.path(), "template.jar", &zip_stored(&[(FORGE_META, TEMPLATE_TOML)]));
        assert!(
            read_mod_meta(&path).is_none(),
            "全是占位符的 mods.toml 应当视为读不出来"
        );
    }

    #[test]
    fn template_placeholder_falls_through_to_manifest() {
        // 模板在前、manifest 在后：应当跳过模板、用 manifest 的打包信息。
        let manifest = b"Manifest-Version: 1.0\r\nImplementation-Title: Decocraft\r\nImplementation-Version: 3.0.7\r\n";
        let dir = tempfile::tempdir().unwrap();
        let path = write_jar(
            dir.path(),
            "template-and-manifest.jar",
            &zip_stored(&[(FORGE_META, TEMPLATE_TOML), (MANIFEST_META, manifest)]),
        );

        let meta = read_mod_meta(&path).expect("manifest 兜底应当能用");
        assert_eq!(meta.source, "manifest");
        assert_eq!(meta.name.as_deref(), Some("Decocraft"));
        assert_eq!(meta.version.as_deref(), Some("3.0.7"));
    }
}
