//! 版本扫描与分类（`VersionStoreService`）。
//!
//! 判定规则（与侧栏分组一一对应）：
//! - 版本 JSON 里出现 `net.fabricmc` → **Fabric**
//! - 出现 `net.neoforged` / `neoforge` → **NeoForge**
//! - 出现 `net.minecraftforge` → **Forge**
//! - 否则若没有 `inheritsFrom` → **原版**
//! - 其余（Quilt / 残缺）→ **未识别**，v1 只提供「打开目录」

use std::path::{Path, PathBuf};

use serde_json::Value;

use kaleid_api::{
    ApiError, ApiResult, IntegrityReport, LoaderKind, Readiness, VersionStoreService,
    VersionSummary,
};

pub struct VersionStore;

impl VersionStore {
    pub fn new() -> Self {
        VersionStore
    }

    /// 解析版本 JSON 的绝对路径（MC 约定：`versions/<id>/<id>.json`）。
    pub fn json_path(root: &Path, version_id: &str) -> PathBuf {
        root.join("versions")
            .join(version_id)
            .join(format!("{version_id}.json"))
    }

    /// 递归合并 `inheritsFrom` 链，返回从父到子的 JSON 列表。
    pub fn load_chain(&self, root: &Path, version_id: &str) -> ApiResult<Vec<Value>> {
        let mut chain = Vec::new();
        let mut current = version_id.to_string();
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 16 {
                return Err(ApiError::Format(format!(
                    "版本 {version_id} 的 inheritsFrom 链条过深或成环"
                )));
            }
            let path = Self::json_path(root, &current);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ApiError::not_found(format!("{}（{e}）", path.display())))?;
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| ApiError::Format(format!("{}: {e}", path.display())))?;
            let parent = value
                .get("inheritsFrom")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            chain.push(value);
            match parent {
                Some(p) if !p.is_empty() => current = p,
                _ => break,
            }
        }
        chain.reverse();
        Ok(chain)
    }

    /// 完整校验：多查一遍 `assets/objects` 里的每个文件。
    ///
    /// 日常扫描只查索引文件本身（一个版本扫三五千个对象太贵）；用户点「校验」时
    /// 走这里，换来"到底缺几个资源文件"的准确答案。**全部从 `.minecraft` 根读。**
    pub fn verify(&self, root: &Path, version_id: &str) -> ApiResult<Integrity> {
        let chain = self.load_chain(root, version_id)?;
        let mut integrity = check_integrity(root, version_id, &chain);
        let Some(index) = asset_index_id(&chain) else {
            return Ok(integrity);
        };
        let path = root
            .join("assets")
            .join("indexes")
            .join(format!("{index}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(integrity);
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Ok(integrity);
        };
        let Some(objects) = value.get("objects").and_then(|o| o.as_object()) else {
            return Ok(integrity);
        };
        integrity.assets_object_total = objects.len();
        let objects_dir = root.join("assets").join("objects");
        for object in objects.values() {
            let Some(hash) = object.get("hash").and_then(|h| h.as_str()) else {
                continue;
            };
            if hash.len() < 2 {
                continue;
            }
            if !objects_dir.join(&hash[..2]).join(hash).is_file() {
                integrity.missing_assets_objects += 1;
            }
        }
        Ok(integrity)
    }
}

impl Default for VersionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn libraries_blob(value: &Value) -> String {
    let mut buf = String::new();
    if let Some(libs) = value.get("libraries").and_then(|v| v.as_array()) {
        for lib in libs {
            if let Some(name) = lib.get("name").and_then(|v| v.as_str()) {
                buf.push_str(&name.to_ascii_lowercase());
                buf.push(' ');
            }
            if let Some(url) = lib
                .get("downloads")
                .and_then(|d| d.get("artifact"))
                .and_then(|a| a.get("url"))
                .and_then(|v| v.as_str())
            {
                buf.push_str(&url.to_ascii_lowercase());
                buf.push(' ');
            }
        }
    }
    if let Some(main) = value.get("mainClass").and_then(|v| v.as_str()) {
        buf.push_str(&main.to_ascii_lowercase());
    }
    buf
}

fn classify(value: &Value) -> LoaderKind {
    let blob = libraries_blob(value);
    // 顺序有讲究：NeoForge 自己也依赖 `net.fabricmc:sponge-mixin`，所以"看到
    // net.fabricmc 就是 Fabric"会把 NeoForge 版本认错（真机上就认错过，模组列表
    // 会按 Fabric 去筛）。先认更具体的 NeoForge / Forge，Fabric 只认 fabric-loader。
    if blob.contains("net.neoforged") || blob.contains("neoforge") {
        return LoaderKind::NeoForge;
    }
    if blob.contains("net.minecraftforge") {
        return LoaderKind::Forge;
    }
    if blob.contains("fabric-loader") || blob.contains("net.fabricmc") {
        return LoaderKind::Fabric;
    }
    let has_parent = value
        .get("inheritsFrom")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if has_parent {
        LoaderKind::Unrecognized
    } else {
        LoaderKind::Vanilla
    }
}

/// 版本目录里的 JSON 不一定叫 `<目录名>.json`：其它启动器写出来的 NeoForge /
/// Forge 版本常带自己的命名（`neoforge-21.1.72.json`）。找不到同名文件时退而求其次。
fn resolve_version_json(dir: &Path, dir_name: &str) -> Option<PathBuf> {
    let expected = dir.join(format!("{dir_name}.json"));
    if expected.is_file() {
        return Some(expected);
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .map(|ext| ext.eq_ignore_ascii_case("json"))
                .unwrap_or(false)
        })
        .collect();
    candidates.sort();
    if candidates.is_empty() {
        return None;
    }
    // 多份时优先 `id` 字段正好等于目录名的那份，避免误读 profile 之类的附属 JSON。
    for path in &candidates {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("id").and_then(|v| v.as_str()) == Some(dir_name) {
            return Some(path.clone());
        }
    }
    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

/// 加载器标记（`.fabric` / `.neoforge` / `.forge` 下的 `loader.json`）。
/// 扫描时它比库坐标更可靠：NeoForge 也依赖 `net.fabricmc:sponge-mixin`。
fn read_loader_marker(dir: &Path) -> Option<(LoaderKind, Option<String>)> {
    for (kind, marker) in [
        (LoaderKind::Fabric, ".fabric"),
        (LoaderKind::NeoForge, ".neoforge"),
        (LoaderKind::Forge, ".forge"),
    ] {
        let path = dir.join(marker).join("loader.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            return Some((
                kind,
                value.get("loader").and_then(|v| v.as_str()).map(str::to_string),
            ));
        }
    }
    None
}

/// 标记里记的原版版本。合并后的 JSON 没有 `inheritsFrom` 时靠它。
fn marker_game(dir: &Path) -> Option<String> {
    for marker in [".fabric", ".neoforge", ".forge"] {
        let path = dir.join(marker).join("loader.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if let Some(game) = value
                .get("game")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(game.to_string());
            }
        }
    }
    None
}

fn loader_version(value: &Value, loader: LoaderKind) -> Option<String> {
    let prefix = match loader {
        LoaderKind::Fabric => "net.fabricmc:fabric-loader:",
        LoaderKind::Forge => "net.minecraftforge:forge:",
        LoaderKind::NeoForge => "net.neoforged:neoforge:",
        _ => return None,
    };
    let libs = value.get("libraries").and_then(|v| v.as_array())?;
    for lib in libs {
        let name = lib.get("name").and_then(|v| v.as_str())?;
        if let Some(rest) = name.strip_prefix(prefix) {
            // Forge 的坐标带 classifier：`net.minecraftforge:forge:1.21.4-54.1.22:universal`。
            return Some(rest.split(':').next().unwrap_or(rest).to_string());
        }
    }
    // NeoForge 早期命名 `net.neoforged:forge:`
    if loader == LoaderKind::NeoForge {
        for lib in libs {
            let name = lib.get("name").and_then(|v| v.as_str())?;
            if let Some(rest) = name.strip_prefix("net.neoforged:forge:") {
                return Some(rest.split(':').next().unwrap_or(rest).to_string());
            }
        }
    }
    None
}

/// 从版本 JSON 链里取资源索引 id（`assetIndex.id`；老版本 JSON 用 `assets`）。
pub fn asset_index_id(chain: &[Value]) -> Option<String> {
    chain
        .iter()
        .filter_map(|v| {
            v.get("assetIndex")
                .and_then(|a| a.get("id"))
                .and_then(|m| m.as_str())
        })
        .next_back()
        .map(|s| s.to_string())
        .or_else(|| {
            chain
                .iter()
                .filter_map(|v| v.get("assets").and_then(|m| m.as_str()))
                .next_back()
                .map(|s| s.to_string())
        })
}

/// 一次完整性检查的明细。
///
/// `missing_assets_objects` 只有 [`VersionStore::verify`] 会去数：一个资源索引里有
/// 三五千个对象，逐个 `stat` 太贵，日常扫描只查索引文件本身。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Integrity {
    pub missing_jar: bool,
    pub missing_libraries: usize,
    pub missing_assets_index: bool,
    pub missing_assets_objects: usize,
    pub assets_object_total: usize,
}

impl Integrity {
    /// 缺失项总数（界面上报"缺少 N 个文件"）。
    pub fn missing(&self) -> usize {
        usize::from(self.missing_jar)
            + self.missing_libraries
            + usize::from(self.missing_assets_index)
            + self.missing_assets_objects
    }

    pub fn ok(&self) -> bool {
        self.missing() == 0
    }

    /// 转成给界面的报告。
    pub fn report(&self, version_id: &str) -> IntegrityReport {
        IntegrityReport {
            version_id: version_id.to_string(),
            ok: self.ok(),
            missing_jar: self.missing_jar,
            missing_libraries: self.missing_libraries,
            missing_assets_index: self.missing_assets_index,
            missing_assets_objects: self.missing_assets_objects,
            assets_object_total: self.assets_object_total,
            missing_total: self.missing(),
        }
    }
}

/// 统计版本是否完整。
///
/// # 位置规则（重要）
///
/// jar / 依赖库 / 资源文件**全部从 `.minecraft` 根目录读**：
/// `versions/<id>/<id>.jar`、`libraries/…`、`assets/…`。
/// 版本隔离只影响 `--gameDir`（存档、模组、日志），**不影响**校验来源——
/// 隔离开的版本依然共用根目录下的 libraries 与 assets。
/// 唯一"在版本目录里"的就是该版本自己的客户端 jar，因为它本来就只在那里。
///
/// 继承版本自己没有 jar，需要顺着 `inheritsFrom` 找到真正的 jar。
fn check_integrity(root: &Path, version_id: &str, chain: &[Value]) -> Integrity {
    let mut out = Integrity::default();

    let mut jar_found = false;
    let mut cursor = Some(version_id.to_string());
    let mut guard = 0;
    while let Some(id) = cursor.take() {
        guard += 1;
        if guard > 16 {
            break;
        }
        let jar = root.join("versions").join(&id).join(format!("{id}.jar"));
        if jar.is_file() {
            jar_found = true;
            break;
        }
        cursor = chain
            .iter()
            .find(|v| v.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
            .and_then(|v| v.get("inheritsFrom"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }
    out.missing_jar = !jar_found;

    let libraries = root.join("libraries");
    // 自包含安装把加载器的库放在 `versions/<id>/libraries/`，和拼 classpath 时一致。
    let version_libraries = root.join("versions").join(version_id).join("libraries");
    for value in chain {
        let Some(libs) = value.get("libraries").and_then(|v| v.as_array()) else {
            continue;
        };
        for lib in libs {
            // 别的系统的库（`natives-linux` / `natives-macos` 之类）不算缺失，
            // 否则每个版本都会报"缺几十个文件"。
            let allowed = lib
                .get("rules")
                .and_then(|r| r.as_array())
                .map(|r| crate::launch::rules_allow(r))
                .unwrap_or(true);
            if !allowed {
                continue;
            }
            let Some(path) = crate::install::library_relative_path(lib) else {
                continue;
            };
            let in_version = version_libraries.join(&path).is_file();
            if !in_version && !libraries.join(&path).is_file() {
                out.missing_libraries += 1;
            }
        }
    }

    if let Some(index) = asset_index_id(chain) {
        out.missing_assets_index = !root
            .join("assets")
            .join("indexes")
            .join(format!("{index}.json"))
            .is_file();
    }

    out
}

impl VersionStoreService for VersionStore {
    fn scan(&self, root_path: &Path) -> ApiResult<Vec<VersionSummary>> {
        let versions_dir = root_path.join("versions");
        if !versions_dir.is_dir() {
            // 空目录 / 还没创建 .minecraft 的文件夹都走这里，返回空列表而不是报错
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let entries = std::fs::read_dir(&versions_dir)
            .map_err(|e| ApiError::io(format!("{}: {e}", versions_dir.display())))?;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let Some(json_path) = resolve_version_json(&entry.path(), &dir_name) else {
                continue;
            };
            match self.inspect(&json_path) {
                Ok(summary) => out.push(summary),
                // 单个版本 JSON 损坏不应该让整个列表消失
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| natural_cmp(&b.id, &a.id));
        Ok(out)
    }

    fn inspect(&self, json_path: &Path) -> ApiResult<VersionSummary> {
        let text = std::fs::read_to_string(json_path)
            .map_err(|e| ApiError::io(format!("{}: {e}", json_path.display())))?;
        let parsed: Option<Value> = serde_json::from_str(&text).ok();

        let version_dir = json_path
            .parent()
            .ok_or_else(|| ApiError::invalid("路径缺少父目录"))?;
        let dir_name = version_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let marker = read_loader_marker(version_dir);
        // JSON 写坏但加载器标记还在时，仍然把这个版本列出来（标成未就绪），
        // 否则「装到一半 / 被别的启动器写坏」的 NeoForge 会直接从侧栏消失。
        let value = match parsed {
            Some(value) => value,
            None if marker.is_some() => Value::Null,
            None => {
                return Err(ApiError::Format(format!("{}: JSON 无法解析", json_path.display())))
            }
        };
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or(dir_name.clone());

        let inherits_from = value
            .get("inheritsFrom")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        // 标记优先：NeoForge 自己依赖 `net.fabricmc:sponge-mixin`，只靠库名会认错。
        let (loader, marker_version) = match &marker {
            Some((kind, version)) => (*kind, version.clone()),
            None => (classify(&value), None),
        };
        let loader_version = marker_version.or_else(|| loader_version(&value, loader)).map(|version| {
            // Forge 的 Maven 坐标是 `1.21.4-54.1.22`，而安装请求只带 `54.1.22`。
            if loader == LoaderKind::Forge {
                if let Some(game) = inherits_from.as_deref() {
                    if let Some(short) = version.strip_prefix(&format!("{game}-")) {
                        return short.to_string();
                    }
                }
            }
            version
        });
        let java_major = value
            .get("javaVersion")
            .and_then(|v| v.get("majorVersion"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let root = version_dir
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| ApiError::invalid("无法定位 .minecraft 根目录"))?;
        let chain = self.load_chain(root, &id).unwrap_or_else(|_| vec![value]);

        let integrity = check_integrity(root, &id, &chain);
        let readiness = if integrity.ok() {
            Readiness::Ready
        } else {
            Readiness::Incomplete {
                missing: integrity.missing(),
            }
        };

        let last_modified = std::fs::metadata(json_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().to_string());

        Ok(VersionSummary {
            id,
            loader,
            loader_version,
            inherits_from,
            game_version: marker_game(version_dir).or_else(|| base_game_version(&chain, &dir_name)),
            readiness,
            java_major,
            json_path: json_path.to_string_lossy().to_string(),
            last_modified,
        })
    }
}

/// 版本对应的基础原版号。
///
/// 顺序：安装时写入的 `kaleidBaseGameVersion`（自包含布局唯一的来源）→ 链上
/// 声明了 `downloads.client` / `assets` 的那一层的 `id` → 兜底用版本自己的 id。
///
/// 之所以要一个自定义键：合并后的加载器 JSON 里没有 `inheritsFrom`，而模组热门
/// 列表与更新检查都必须知道"这是哪个原版版本"。
pub fn base_game_version(chain: &[Value], version_id: &str) -> Option<String> {
    for value in chain.iter().rev() {
        if let Some(base) = value
            .get("kaleidBaseGameVersion")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(base.to_string());
        }
    }
    for value in chain.iter().rev() {
        let looks_vanilla = value.get("downloads").and_then(|d| d.get("client")).is_some()
            || value.get("assetIndex").is_some()
            || value.get("assets").is_some();
        if !looks_vanilla {
            continue;
        }
        if let Some(id) = value
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(id.to_string());
        }
    }
    Some(version_id.to_string())
}

/// 粗略的"自然序"比较：`1.21.4` 排在 `1.21` 之前，`1.9` 排在 `1.10` 之后。
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> Vec<String> {
        s.split(|c: char| c == '.' || c == '-' || c == '_')
            .map(|p| p.to_string())
            .collect()
    };
    let pa = split(a);
    let pb = split(b);
    for i in 0..pa.len().max(pb.len()) {
        let sa = pa.get(i).cloned().unwrap_or_default();
        let sb = pb.get(i).cloned().unwrap_or_default();
        let na = sa.parse::<u64>();
        let nb = sb.parse::<u64>();
        let ord = match (na, nb) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => sa.cmp(&sb),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    fn make_fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".minecraft");
        std::fs::create_dir_all(root.join("versions")).unwrap();
        std::fs::create_dir_all(root.join("libraries")).unwrap();
        Fixture { _dir: dir, root }
    }

    fn write_version(root: &Path, id: &str, value: Value, with_jar: bool, libs: &[&str]) {
        let dir = root.join("versions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
        if with_jar {
            std::fs::write(dir.join(format!("{id}.jar")), b"jar").unwrap();
        }
        for lib in libs {
            let p = root.join("libraries").join(lib);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"lib").unwrap();
        }
    }

    #[test]
    fn classifies_vanilla_fabric_neoforge_and_unrecognized() {
        let f = make_fixture();
        write_version(
            &f.root,
            "1.21.4",
            json!({"id": "1.21.4", "javaVersion": {"majorVersion": 21}}),
            true,
            &[],
        );
        write_version(
            &f.root,
            "1.21.4-fabric",
            json!({
                "id": "1.21.4-fabric",
                "inheritsFrom": "1.21.4",
                "libraries": [{"name": "net.fabricmc:fabric-loader:0.16.9"}]
            }),
            false,
            &[],
        );
        write_version(
            &f.root,
            "1.21.1-neoforge",
            json!({
                "id": "1.21.1-neoforge",
                "inheritsFrom": "1.21.1",
                "libraries": [{"name": "net.neoforged:neoforge:21.1.66"}]
            }),
            false,
            &[],
        );
        write_version(
            &f.root,
            "1.20.1-forge",
            json!({
                "id": "1.20.1-forge",
                "inheritsFrom": "1.20.1",
                "libraries": [{"name": "net.minecraftforge:forge:1.20.1-47.2.0:universal"}]
            }),
            false,
            &[],
        );

        let store = VersionStore::new();
        let mut list = store.scan(&f.root).unwrap();
        list.sort_by(|a, b| a.id.cmp(&b.id));

        let by_id = |id: &str| list.iter().find(|v| v.id == id).unwrap().clone();
        assert_eq!(by_id("1.21.4").loader, LoaderKind::Vanilla);
        assert_eq!(by_id("1.21.4").java_major, Some(21));
        assert_eq!(by_id("1.21.4-fabric").loader, LoaderKind::Fabric);
        assert_eq!(
            by_id("1.21.4-fabric").loader_version.as_deref(),
            Some("0.16.9")
        );
        assert_eq!(by_id("1.21.1-neoforge").loader, LoaderKind::NeoForge);
        assert_eq!(
            by_id("1.21.1-neoforge").loader_version.as_deref(),
            Some("21.1.66")
        );
        assert_eq!(by_id("1.20.1-forge").loader, LoaderKind::Forge);
        assert_eq!(
            by_id("1.20.1-forge").loader_version.as_deref(),
            Some("47.2.0")
        );
    }

    #[test]
    fn readiness_reports_missing_jar_and_libraries() {
        let f = make_fixture();
        write_version(&f.root, "1.21.4", json!({"id": "1.21.4"}), true, &[]);
        write_version(
            &f.root,
            "1.19.2",
            json!({
                "id": "1.19.2",
                "libraries": [
                    {"name": "net.java:jopt:5", "downloads": {"artifact": {"path": "net/java/jopt/5/jopt-5.jar"}}},
                    {"name": "net.java:other:1", "downloads": {"artifact": {"path": "net/java/other/1/other-1.jar"}}}
                ]
            }),
            false,
            &["net/java/jopt/5/jopt-5.jar"],
        );

        let store = VersionStore::new();
        let list = store.scan(&f.root).unwrap();
        let by_id = |id: &str| list.iter().find(|v| v.id == id).unwrap().clone();
        assert_eq!(by_id("1.21.4").readiness, Readiness::Ready);
        // 缺 jar(1) + 缺 other(1) = 2
        assert_eq!(
            by_id("1.19.2").readiness,
            Readiness::Incomplete { missing: 2 }
        );
    }

    #[test]
    fn inherited_version_reuses_parent_jar() {
        let f = make_fixture();
        write_version(&f.root, "1.21.4", json!({"id": "1.21.4"}), true, &[]);
        write_version(
            &f.root,
            "1.21.4-fabric",
            json!({
                "id": "1.21.4-fabric",
                "inheritsFrom": "1.21.4",
                "libraries": [{"name": "net.fabricmc:fabric-loader:0.16.9"}]
            }),
            false,
            // 只有 Maven 坐标的库也会按坐标推出路径来查（见 `library_relative_path`），
            // 所以这里得真的把文件放好才算"齐"。
            &["net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"],
        );
        let store = VersionStore::new();
        let list = store.scan(&f.root).unwrap();
        let fabric = list.iter().find(|v| v.id == "1.21.4-fabric").unwrap();
        assert_eq!(fabric.readiness, Readiness::Ready);
        assert_eq!(fabric.inherits_from.as_deref(), Some("1.21.4"));
    }

    #[test]
    fn missing_versions_dir_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = VersionStore::new();
        // 空目录（还没建 .minecraft）
        let list = store.scan(dir.path()).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn broken_version_json_is_skipped_not_fatal() {
        let f = make_fixture();
        write_version(&f.root, "1.21.4", json!({"id": "1.21.4"}), true, &[]);
        let broken = f.root.join("versions/broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("broken.json"), b"{ oops").unwrap();

        let store = VersionStore::new();
        let list = store.scan(&f.root).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "1.21.4");
    }

    /// 其它启动器写出来的 NeoForge 版本：JSON 文件名不等于目录名，加载器只记在标记里。
    ///
    /// 旧扫描只认 `versions/<目录>/<目录>.json`，这类版本会整条消失——用户看到的就是
    /// 「NeoForge 读不出来」。
    #[test]
    fn neoforge_with_marker_and_odd_json_name_is_listed() {
        let f = make_fixture();
        let dir = f.root.join("versions/26.1.2-NeoForge");
        std::fs::create_dir_all(dir.join(".neoforge")).unwrap();
        std::fs::write(
            dir.join(".neoforge/loader.json"),
            json!({"kind": "neoforge", "loader": "26.1.2.125", "game": "26.1.2"}).to_string(),
        )
        .unwrap();
        // 文件名与目录名不同，内容里也没有 neoforge 库坐标。
        std::fs::write(
            dir.join("neoforge-26.1.2.125.json"),
            json!({"id": "26.1.2-NeoForge"}).to_string(),
        )
        .unwrap();

        let store = VersionStore::new();
        let list = store.scan(&f.root).unwrap();
        let neo = list
            .iter()
            .find(|v| v.id == "26.1.2-NeoForge")
            .expect("带标记的 NeoForge 版本必须能被列出");
        assert_eq!(neo.loader, LoaderKind::NeoForge);
        assert_eq!(neo.loader_version.as_deref(), Some("26.1.2.125"));
        assert_eq!(neo.game_version.as_deref(), Some("26.1.2"));
    }

    /// JSON 写坏但标记还在：仍然列出（标成未就绪），不要从 NeoForge 分组里消失。
    #[test]
    fn neoforge_with_broken_json_still_listed_via_marker() {
        let f = make_fixture();
        let dir = f.root.join("versions/26.3-NeoForge");
        std::fs::create_dir_all(dir.join(".neoforge")).unwrap();
        std::fs::write(
            dir.join(".neoforge/loader.json"),
            json!({"kind": "neoforge", "loader": "26.3.0.99", "game": "26.3"}).to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("26.3-NeoForge.json"), b"{ broken").unwrap();

        let store = VersionStore::new();
        let list = store.scan(&f.root).unwrap();
        let neo = list
            .iter()
            .find(|v| v.id == "26.3-NeoForge")
            .expect("标记可读时不该整条丢掉");
        assert_eq!(neo.loader, LoaderKind::NeoForge);
        assert!(matches!(neo.readiness, Readiness::Incomplete { .. }));
    }

    #[test]
    fn natural_order_puts_newer_first() {
        assert_eq!(natural_cmp("1.21", "1.9"), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp("1.21.4", "1.21"), std::cmp::Ordering::Greater);
    }

    /// 校验认 `.minecraft` 根目录，也认 `versions/<id>/libraries/`。
    ///
    /// 后者是自包含安装布局（加载器的库就放在那儿，启动时也先查它），前者是
    /// 多版本共享的依赖库。资源与索引则永远只认根目录。
    #[test]
    fn integrity_reads_minecraft_root_not_version_dir() {
        let f = make_fixture();
        write_version(
            &f.root,
            "1.21.4",
            json!({
                "id": "1.21.4",
                "assetIndex": {"id": "17"},
                "libraries": [
                    {"name": "a:b:1", "downloads": {"artifact": {"path": "a/b/1/b-1.jar"}}}
                ]
            }),
            true,
            &[],
        );
        let json_path = VersionStore::json_path(&f.root, "1.21.4");
        let store = VersionStore::new();

        let stray = f.root.join("versions").join("1.21.4");
        std::fs::create_dir_all(stray.join("libraries/a/b/1")).unwrap();
        std::fs::write(stray.join("libraries/a/b/1/b-1.jar"), b"x").unwrap();
        std::fs::create_dir_all(stray.join("assets/indexes")).unwrap();
        std::fs::write(stray.join("assets/indexes/17.json"), b"{}").unwrap();

        // 依赖库放进版本目录里的 `libraries/`：算齐了（自包含布局，启动时也是先查这里）；
        // 但 `assets/` 放错地方不算数，所以还差资源索引这 1 项。
        assert_eq!(
            store.inspect(&json_path).unwrap().readiness,
            Readiness::Incomplete { missing: 1 }
        );

        // 放进 .minecraft 根目录：才算齐了
        let lib = f.root.join("libraries/a/b/1");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("b-1.jar"), b"x").unwrap();
        std::fs::create_dir_all(f.root.join("assets/indexes")).unwrap();
        std::fs::write(f.root.join("assets/indexes/17.json"), b"{}").unwrap();

        assert_eq!(
            store.inspect(&json_path).unwrap().readiness,
            Readiness::Ready
        );
    }

    #[test]
    fn verify_counts_missing_assets_objects() {
        let f = make_fixture();
        write_version(
            &f.root,
            "1.21.4",
            json!({"id": "1.21.4", "assetIndex": {"id": "17"}}),
            true,
            &[],
        );
        std::fs::create_dir_all(f.root.join("assets/indexes")).unwrap();
        std::fs::write(
            f.root.join("assets/indexes/17.json"),
            br#"{"objects":{"a/a.txt":{"hash":"aaaa","size":1},"b/b.txt":{"hash":"bbbb","size":1}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(f.root.join("assets/objects/aa")).unwrap();
        std::fs::write(f.root.join("assets/objects/aa/aaaa"), b"x").unwrap();

        let store = VersionStore::new();
        let report = store.verify(&f.root, "1.21.4").unwrap().report("1.21.4");
        assert_eq!(report.assets_object_total, 2);
        assert_eq!(report.missing_assets_objects, 1);
        assert!(!report.missing_assets_index);
        assert!(!report.missing_jar);
        assert_eq!(report.missing_libraries, 0);
        assert!(!report.ok);
        assert_eq!(report.missing_total, 1);
    }
}
