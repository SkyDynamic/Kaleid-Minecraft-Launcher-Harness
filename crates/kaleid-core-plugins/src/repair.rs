//! 启动前补全：算清楚「为了让这个版本能启动，还缺哪些文件」。
//!
//! 只读文件系统，不发任何请求——要下什么交给 `DownloadManager`。这样补全逻辑
//! 可以单测，也不会出现"检查完整性时顺手写了半个文件"的事。
//!
//! 覆盖四类文件，和 [`crate::versions::check_integrity`] 的口径保持一致：
//! 客户端 jar、依赖库、资源索引、索引里登记的资源对象。

use std::path::{Path, PathBuf};

use serde_json::Value;

use kaleid_api::{ApiError, ApiResult, LaunchPlan};

use crate::downloads::DownloadItem;
use crate::install::{artifact_key, rules_allow, library_relative_path};

/// 资源对象与资源索引的官方地址（Mojang 的 CDN）。
const RESOURCES_BASE: &str = "https://resources.download.minecraft.net";

/// 一个版本要补的文件清单。
#[derive(Debug, Clone, Default)]
pub struct RepairPlan {
    pub items: Vec<DownloadItem>,
    /// 待下载字节总数（界面的进度条分母）。
    pub total_bytes: u64,
}

impl RepairPlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn files(&self) -> usize {
        self.items.len()
    }
}

/// 把 `plan` 对应的版本链读出来（子版本在前）。
///
/// 读不到版本 JSON 不是错误：这种情况下没有可补的东西，交给启动进程自己报
/// 「版本不可用」。所以这里返回空链而不是 `Err`。
pub fn version_chain(mc_dir: &Path, version_id: &str) -> Vec<(String, Value)> {
    let mut chain: Vec<(String, Value)> = Vec::new();
    let mut current = version_id.to_string();
    for _ in 0..8 {
        let json = mc_dir
            .join("versions")
            .join(&current)
            .join(format!("{current}.json"));
        let Ok(text) = std::fs::read_to_string(&json) else {
            break;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            break;
        };
        let parent = value
            .get("inheritsFrom")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        chain.push((current.clone(), value));
        if parent.is_empty() || parent == current {
            break;
        }
        current = parent;
    }
    chain
}

/// `.minecraft` 根目录：优先用计划里带的，老计划退回 `libraries_dir` 的父目录。
fn mc_root(plan: &LaunchPlan) -> PathBuf {
    if !plan.mc_dir.is_empty() {
        return PathBuf::from(&plan.mc_dir);
    }
    Path::new(&plan.libraries_dir)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 算出缺什么。`plan` 里已有的文件一律不返回。
pub fn missing_items(plan: &LaunchPlan) -> ApiResult<RepairPlan> {
    let mc = mc_root(plan);
    let chain = version_chain(&mc, &plan.version_id);
    let mut items: Vec<DownloadItem> = Vec::new();

    let libraries_dirs: Vec<PathBuf> = if plan.libraries_dirs.is_empty() {
        vec![PathBuf::from(&plan.libraries_dir)]
    } else {
        plan.libraries_dirs.iter().map(PathBuf::from).collect()
    };
    // 新文件优先落到版本本地目录（自包含布局），没有再落根 libraries/。
    let lib_target = libraries_dirs
        .first()
        .cloned()
        .unwrap_or_else(|| mc.join("libraries"));

    // ---- 1) 客户端 jar ----
    let own_jar = mc
        .join("versions")
        .join(&plan.version_id)
        .join(format!("{}.jar", plan.version_id));
    if !own_jar.is_file() {
        // 顺着链条找声明 downloads.client 的那一层（老的分离布局）。
        if let Some((owner, client)) = chain.iter().find_map(|(id, value)| {
            value
                .get("downloads")
                .and_then(|d| d.get("client"))
                .map(|c| (id.clone(), c.clone()))
        }) {
            let url = client
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !url.is_empty() {
                items.push(item(
                    &plan.version_id,
                    "client",
                    &url,
                    mc.join("versions")
                        .join(&plan.version_id)
                        .join(format!("{}.jar", plan.version_id)),
                    client.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    client.get("sha1").and_then(|v| v.as_str()).map(str::to_string),
                ));
            }
            let _ = owner;
        }
    }

    // ---- 2) 依赖库 ----
    // 同一个库在链条里可能被声明多次，按身份键去重（后面的覆盖前面的），
    // 和拼 classpath 的规则完全一致。
    let mut libs: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    for (_, value) in chain.iter() {
        let Some(list) = value.get("libraries").and_then(|v| v.as_array()) else {
            continue;
        };
        for lib in list {
            let allowed = lib
                .get("rules")
                .and_then(|r| r.as_array())
                .map(|r| rules_allow(r))
                .unwrap_or(true);
            if !allowed {
                continue;
            }
            let name = lib.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            libs.insert(artifact_key(name), lib.clone());
        }
    }
    for lib in libs.values() {
        let Some(rel) = library_relative_path(lib) else {
            continue;
        };
        if libraries_dirs.iter().any(|dir| dir.join(&rel).is_file()) {
            continue;
        }
        let artifact = lib.get("downloads").and_then(|d| d.get("artifact"));
        let url = artifact
            .and_then(|a| a.get("url"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            // 只有坐标 + `url` 仓库前缀的加载器 profile：自己拼出 maven 地址。
            .or_else(|| {
                lib.get("url")
                    .and_then(|v| v.as_str())
                    .filter(|u| !u.is_empty())
                    .map(|base| format!("{}/{rel}", base.trim_end_matches('/')))
            });
        let Some(url) = url else {
            continue;
        };
        items.push(item(
            &plan.version_id,
            "libraries",
            &url,
            lib_target.join(&rel),
            artifact
                .and_then(|a| a.get("size"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            artifact
                .and_then(|a| a.get("sha1"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
        ));
    }

    // ---- 2.5) 原生库 ----
    // 1.12.2 及更早把原生库放在 `downloads.classifiers` 里，不在上面的 artifact
    // 收集里。漏了它们，启动会以 `no lwjgl in java.library.path` 收场，而"缺什么"
    // 的清单上却一个字都没有。
    for native in &plan.natives_jars {
        if libraries_dirs
            .iter()
            .any(|dir| dir.join(&native.path).is_file())
        {
            continue;
        }
        let url = native
            .url
            .clone()
            .unwrap_or_else(|| format!("https://libraries.minecraft.net/{}", native.path));
        items.push(item(
            &plan.version_id,
            "libraries",
            &url,
            lib_target.join(&native.path),
            native.size,
            native.sha1.clone(),
        ));
    }

    // ---- 3) 资源索引 ----
    let assets = mc.join("assets");
    let (index_id, index_value) = chain
        .iter()
        .find_map(|(_, value)| {
            value
                .get("assetIndex")
                .map(|index| (value.get("id").and_then(|v| v.as_str()).unwrap_or_default(), index))
        })
        .map(|(_, index)| {
            (
                index
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                index.clone(),
            )
        })
        .unwrap_or_default();
    if !index_id.is_empty() {
        let index_path = assets.join("indexes").join(format!("{index_id}.json"));
        if !index_path.is_file() {
            let url = index_value
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !url.is_empty() {
                items.push(item(
                    &plan.version_id,
                    "assets",
                    &url,
                    index_path.clone(),
                    index_value
                        .get("size")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    index_value
                        .get("sha1")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                ));
            }
        }

        // ---- 4) 资源对象 ----
        // 索引还没下下来时这一轮数不出对象；调用方会用第二轮补上。
        if let Ok(text) = std::fs::read_to_string(&index_path) {
            if let Ok(index) = serde_json::from_str::<Value>(&text) {
                if let Some(objects) = index.get("objects").and_then(|v| v.as_object()) {
                    for (name, meta) in objects {
                        let hash = meta.get("hash").and_then(|v| v.as_str()).unwrap_or_default();
                        // 老版本的索引里 hash 是字段而不是对象，跳过这种形状。
                        if hash.len() < 2 {
                            continue;
                        }
                        let prefix = &hash[..2];
                        let dest = assets.join("objects").join(prefix).join(hash);
                        if dest.is_file() {
                            continue;
                        }
                        items.push(item(
                            &plan.version_id,
                            "assets",
                            &format!("{RESOURCES_BASE}/{prefix}/{hash}"),
                            dest,
                            meta.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                            Some(hash.to_string()),
                        ));
                        let _ = name;
                    }
                }
            }
        }
    }

    let total_bytes = items.iter().map(|i| i.size).sum();
    Ok(RepairPlan { items, total_bytes })
}

fn item(
    _version: &str,
    phase: &str,
    url: &str,
    dest: PathBuf,
    size: u64,
    sha1: Option<String>,
) -> DownloadItem {
    DownloadItem {
        label: dest
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| url.to_string()),
        phase: phase.to_string(),
        official_url: Some(url.to_string()),
        // 补全走和安装同一套镜像策略：官方连不上会自动换镜像。
        mirror_url: kaleid_api::mirror_url_for(url),
        fallback_url: None,
        dest,
        size,
        sha1,
    }
}

/// 把 `ApiError` 包装成带上下文的错误（下载失败的提示里要能看到是哪个文件）。
pub fn describe_failure(item_count: usize, error: &ApiError) -> String {
    format!("补全缺失文件失败（{item_count} 个待下载）：{error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn plan_for(mc: &Path, version: &str) -> LaunchPlan {
        LaunchPlan {
            version_id: version.to_string(),
            java_path: "java".into(),
            java_source: "auto".into(),
            game_dir: mc.to_string_lossy().to_string(),
            mc_dir: mc.to_string_lossy().to_string(),
            assets_dir: mc.join("assets").to_string_lossy().to_string(),
            libraries_dir: mc.join("libraries").to_string_lossy().to_string(),
            isolated: false,
            main_class: "net.minecraft.client.main.Main".into(),
            classpath: Vec::new(),
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            extra_jvm_args: Vec::new(),
            username: "Tester".into(),
            uuid: String::new(),
            asset_index: None,
            launcher_brand: None,
            launch_dir: mc.to_string_lossy().to_string(),
            natives_dir: mc.join("natives").to_string_lossy().to_string(),
            natives_jars: Vec::new(),
            libraries_dirs: vec![mc.join("libraries").to_string_lossy().to_string()],
            game_version: version.to_string(),
            access_token_injected: false,
            jvm_notes: Vec::new(),
            auto_chinese: false,
            write_latest_launch_bat: false,
            high_performance_gpu: false,
        }
    }

    fn write_version(mc: &Path, id: &str, value: Value) {
        let dir = mc.join("versions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_string(&value).unwrap(),
        )
        .unwrap();
    }

    /// 一个"齐了"的版本不该报任何缺失。
    #[test]
    fn complete_version_needs_nothing() {
        let dir = tempdir().unwrap();
        let mc = dir.path();
        write_version(
            mc,
            "1.20.1",
            json!({
                "id": "1.20.1",
                "libraries": [
                    {"name": "a:b:1", "downloads": {"artifact": {"path": "a/b/1/b-1.jar"}}}
                ]
            }),
        );
        std::fs::create_dir_all(mc.join("libraries/a/b/1")).unwrap();
        std::fs::write(mc.join("libraries/a/b/1/b-1.jar"), b"x").unwrap();
        std::fs::write(
            mc.join("versions/1.20.1/1.20.1.jar"),
            b"jar",
        )
        .unwrap();

        let plan = plan_for(mc, "1.20.1");
        let repair = missing_items(&plan).unwrap();
        assert!(repair.is_empty(), "不该有缺失：{:?}", repair.items.len());
        assert_eq!(repair.total_bytes, 0);
    }

    /// 分类器形态的原生库（1.12.2 及更早）缺失时也要报出来，并带下载地址。
    #[test]
    fn missing_classifier_natives_are_reported() {
        let dir = tempdir().unwrap();
        let mc = dir.path();
        write_version(mc, "1.5.2", json!({"id": "1.5.2"}));
        std::fs::write(mc.join("versions/1.5.2/1.5.2.jar"), b"jar").unwrap();

        let os = crate::natives::current_os();
        let relative = format!(
            "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-{os}.jar"
        );
        let mut plan = plan_for(mc, "1.5.2");
        plan.natives_jars = vec![kaleid_api::NativeJarRef {
            path: relative.clone(),
            url: Some(format!("https://libraries.minecraft.net/{relative}")),
            sha1: Some("c".repeat(40)),
            size: 578_680,
            excludes: vec!["META-INF/".to_string()],
        }];

        let repair = missing_items(&plan).unwrap();
        assert_eq!(repair.files(), 1, "{:?}", repair.items.len());
        assert!(repair.items[0]
            .dest
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with(&format!("libraries/{relative}")));
        assert_eq!(repair.total_bytes, 578_680, "进度条要有分母");

        // 补上之后就不该再报。
        std::fs::create_dir_all(mc.join("libraries/org/lwjgl/lwjgl/lwjgl-platform/2.9.0"))
            .unwrap();
        std::fs::write(mc.join("libraries").join(&relative), b"jar").unwrap();
        assert!(missing_items(&plan).unwrap().is_empty());
    }

    /// 只有 Maven 坐标（没有 `downloads.artifact`）的库也要能算出下载地址。
    #[test]
    fn coordinate_only_library_gets_its_maven_url() {
        let dir = tempdir().unwrap();
        let mc = dir.path();
        write_version(
            mc,
            "1.21.10",
            json!({
                "id": "1.21.10",
                "libraries": [
                    {"name": "net.fabricmc:fabric-loader:0.18.3", "url": "https://maven.fabricmc.net/"}
                ]
            }),
        );
        std::fs::write(mc.join("versions/1.21.10/1.21.10.jar"), b"jar").unwrap();

        let plan = plan_for(mc, "1.21.10");
        let repair = missing_items(&plan).unwrap();
        assert_eq!(repair.files(), 1);
        let item = &repair.items[0];
        assert_eq!(
            item.official_url.as_deref(),
            Some("https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.18.3/fabric-loader-0.18.3.jar")
        );
        assert!(item
            .dest
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("libraries/net/fabricmc/fabric-loader/0.18.3/fabric-loader-0.18.3.jar"));
    }

    /// 别的系统的 natives 不该被算成缺失。
    #[test]
    fn other_platform_natives_are_ignored() {
        let dir = tempdir().unwrap();
        let mc = dir.path();
        write_version(
            mc,
            "1.16.5",
            json!({
                "id": "1.16.5",
                "libraries": [
                    {
                        "name": "org.lwjgl:lwjgl:3.2.2:natives-linux",
                        "rules": [{"action": "allow", "os": {"name": "linux"}}],
                        "downloads": {"artifact": {"path": "org/lwjgl/lwjgl/3.2.2/lwjgl-3.2.2-natives-linux.jar"}}
                    }
                ]
            }),
        );
        std::fs::write(mc.join("versions/1.16.5/1.16.5.jar"), b"jar").unwrap();

        let plan = plan_for(mc, "1.16.5");
        let repair = missing_items(&plan).unwrap();
        assert!(repair.is_empty(), "Windows 上不该要求 linux natives");
    }

    /// 索引在、对象缺：按 hash 分桶算出每个对象地址。
    #[test]
    fn missing_asset_objects_are_listed_by_hash() {
        let dir = tempdir().unwrap();
        let mc = dir.path();
        write_version(
            mc,
            "1.20.1",
            json!({
                "id": "1.20.1",
                "assetIndex": {"id": "12", "url": "https://example.invalid/12.json", "size": 10, "sha1": "00"}
            }),
        );
        std::fs::write(mc.join("versions/1.20.1/1.20.1.jar"), b"jar").unwrap();
        let indexes = mc.join("assets/indexes");
        std::fs::create_dir_all(&indexes).unwrap();
        std::fs::write(
            indexes.join("12.json"),
            json!({
                "objects": {
                    "minecraft/sounds/a.ogg": {"hash": "abcdef1234", "size": 1234},
                    "minecraft/sounds/b.ogg": {"hash": "ffeedd5678", "size": 99}
                }
            })
            .to_string(),
        )
        .unwrap();
        // 其中一个已经在本地：只该补另一个。
        let have = mc.join("assets/objects/ab/abcdef1234");
        std::fs::create_dir_all(have.parent().unwrap()).unwrap();
        std::fs::write(have, b"x").unwrap();

        let plan = plan_for(mc, "1.20.1");
        let repair = missing_items(&plan).unwrap();
        assert_eq!(repair.files(), 1);
        assert_eq!(repair.total_bytes, 99);
        assert_eq!(
            repair.items[0].official_url.as_deref(),
            Some("https://resources.download.minecraft.net/ff/ffeedd5678")
        );
    }

    /// 索引文件本身缺失时要先补它（对象要等下一轮才数得出来）。
    #[test]
    fn missing_index_is_downloaded_first() {
        let dir = tempdir().unwrap();
        let mc = dir.path();
        write_version(
            mc,
            "1.20.1",
            json!({
                "id": "1.20.1",
                "assetIndex": {"id": "12", "url": "https://example.invalid/12.json", "size": 10, "sha1": "aa"}
            }),
        );
        std::fs::write(mc.join("versions/1.20.1/1.20.1.jar"), b"jar").unwrap();

        let plan = plan_for(mc, "1.20.1");
        let repair = missing_items(&plan).unwrap();
        assert_eq!(repair.files(), 1);
        assert!(repair.items[0]
            .dest
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("assets/indexes/12.json"));
    }
}
