//! 版本管理动作：删除版本、检查/更新模组加载器。
//!
//! 与安装流程分开的理由：安装是"从零造一个目录"，这里是"动一个已经在磁盘上的
//! 目录"，前置校验完全不同（正在运行、被别的版本继承、加载器种类不匹配）。
//! 任何拒绝都以 [`ApiError`] 回给界面，本模块不自己拼中文错误串以外的东西。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use walkdir::WalkDir;

use kaleid_api::{
    ApiError, ApiResult, LoaderKind, LoaderUpdateStatus, Root, VersionDeleteOutcome,
    VersionStoreService,
};

use crate::loaders;
use crate::paths;
use crate::remote::RemoteClient;
use crate::versions::VersionStore;

/// 版本管理动作的实现。
pub struct VersionAdmin {
    versions: Arc<VersionStore>,
    data_root: PathBuf,
    remote: Arc<RemoteClient>,
}

impl VersionAdmin {
    pub fn new(versions: Arc<VersionStore>, data_root: &Path) -> Self {
        VersionAdmin {
            versions,
            data_root: data_root.to_path_buf(),
            remote: Arc::new(RemoteClient::new()),
        }
    }

    /// 删除一个版本目录（以及它的版本设置）。
    ///
    /// 三条拒绝条件都是"删了会连累别人"：版本不存在、被其它版本 `inheritsFrom`
    /// 引用、目录里还有进程占用文件。
    pub fn delete_version(
        &self,
        root: &Root,
        version_id: &str,
    ) -> ApiResult<VersionDeleteOutcome> {
        let root_path = Path::new(&root.path);
        let dir = root_path.join("versions").join(version_id);
        if !dir.is_dir() {
            return Err(ApiError::not_found(format!("版本 {version_id} 不存在")));
        }
        // 被继承的版本不能删：删掉之后子版本连 mainClass 都读不到，启动必崩。
        if let Ok(list) = self.versions.scan(root_path) {
            if let Some(child) = list
                .iter()
                .find(|v| v.inherits_from.as_deref() == Some(version_id))
            {
                return Err(ApiError::invalid(format!(
                    "版本 {} 正在继承它，先处理那个版本再删除",
                    child.id
                )));
            }
        }

        let freed = dir_size(&dir);
        std::fs::remove_dir_all(&dir).map_err(|e| {
            ApiError::io(format!(
                "删除 {} 失败：{e}（如果游戏正在运行，请先退出游戏）",
                dir.display()
            ))
        })?;

        let settings_file = paths::version_settings_file(&self.data_root, &root.id, version_id);
        let removed_settings = if settings_file.is_file() {
            match std::fs::remove_file(&settings_file) {
                Ok(()) => Some(settings_file.to_string_lossy().to_string()),
                // 设置文件删不掉不该让整个删除失败：版本目录已经没了，
                // 残留的 json 没有任何东西会去读它。
                Err(_) => None,
            }
        } else {
            None
        };

        Ok(VersionDeleteOutcome {
            version_id: version_id.to_string(),
            removed_dir: dir.to_string_lossy().to_string(),
            removed_settings,
            freed_bytes: freed,
        })
    }

    /// 读一个版本装了哪个加载器、哪个版本，再问远端有没有更新的。
    pub fn loader_status(&self, root: &Root, version_id: &str) -> ApiResult<LoaderUpdateStatus> {
        let root_path = Path::new(&root.path);
        let chain = self.versions.load_chain(root_path, version_id)?;
        let installed = installed_loader(&chain, root_path, version_id);
        let loader = installed
            .as_ref()
            .map(|i| i.kind)
            .unwrap_or(LoaderKind::Vanilla);
        let current = installed.as_ref().and_then(|i| i.version.clone());
        let game = game_version_of(&chain, version_id);

        let mut status = LoaderUpdateStatus {
            loader,
            current: current.clone(),
            latest: None,
            recommended: None,
            newer_available: false,
            candidates: Vec::new(),
        };
        if loader == LoaderKind::Vanilla {
            return Ok(status);
        }
        // OptiFine 没有"原地换版本"这条路：配合 Forge 时它只是 mods 里的一个 jar，
        // 单独装时画像（tweakClass / libraries）也要一起改。所以这里不给更新提示，
        // 免得界面显示「有新版」、点下去却失败。
        if loader == LoaderKind::OptiFine {
            return Ok(status);
        }
        let Some(game) = game else {
            return Ok(status);
        };
        let list = self.loader_versions(&game, loader)?;
        status.latest = list
            .versions
            .first()
            .map(|v| v.version.clone())
            .or_else(|| current.clone());
        status.recommended = list.recommended.clone();
        status.candidates = list
            .versions
            .iter()
            .filter(|v| v.stable)
            .take(12)
            .map(|v| v.version.clone())
            .collect();
        if status.candidates.is_empty() {
            status.candidates = list
                .versions
                .iter()
                .take(12)
                .map(|v| v.version.clone())
                .collect();
        }
        // "有更新"只在能比较时下结论：远端排序由新到旧，当前版本不在列表里
        // （比如本地是自定义构建）就老老实实说不知道，不猜。
        status.newer_available = match (&status.latest, &current) {
            (Some(latest), Some(current)) => latest != current && list.versions.iter().any(|v| &v.version == latest),
            _ => false,
        };
        Ok(status)
    }

    fn loader_versions(
        &self,
        game: &str,
        loader: LoaderKind,
    ) -> ApiResult<kaleid_api::LoaderVersions> {
        match loader {
            LoaderKind::Fabric => loaders::fabric_loaders(&self.remote, game),
            LoaderKind::Forge => loaders::forge_loaders(&self.remote, game),
            LoaderKind::NeoForge => loaders::neoforge_loaders(&self.remote, game),
            LoaderKind::OptiFine => loaders::optifine_versions(&self.remote, game),
            LoaderKind::Vanilla | LoaderKind::Unrecognized => Ok(kaleid_api::LoaderVersions {
                game_version: game.to_string(),
                loader: loader.slug().to_string(),
                versions: Vec::new(),
                recommended: None,
                fetched_at: 0,
            }),
        }
    }

    /// 装载器更新的「准备」阶段：算出要装哪个版本，把下载任务交给调用方。
    ///
    /// 真正的下载由 [`crate::install::InstallService::update_loader`] 跑，这里只
    /// 负责挑版本 —— 更新和安装共用同一套落盘逻辑，才不会出现两套目录布局。
    pub fn resolve_update_target(
        &self,
        root: &Root,
        version_id: &str,
        requested: Option<&str>,
    ) -> ApiResult<(LoaderKind, String, String)> {
        let status = self.loader_status(root, version_id)?;
        if status.loader == LoaderKind::Vanilla {
            return Err(ApiError::invalid("这个版本没有安装模组加载器"));
        }
        let root_path = Path::new(&root.path);
        let chain = self.versions.load_chain(root_path, version_id)?;
        let game = game_version_of(&chain, version_id)
            .ok_or_else(|| ApiError::Format("读不出这个版本对应的原版游戏版本".into()))?;
        let target = match requested {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => status
                .recommended
                .clone()
                .or(status.latest.clone())
                .ok_or_else(|| ApiError::Network("拿不到可用的加载器版本列表".into()))?,
        };
        if status.current.as_deref() == Some(target.as_str()) {
            return Err(ApiError::invalid(format!(
                "{} {} 已经是当前版本",
                status.loader.label_zh(),
                target
            )));
        }
        Ok((status.loader, game, target))
    }


}

#[derive(Debug, Clone)]
struct InstalledLoader {
    kind: LoaderKind,
    version: Option<String>,
}

/// 从版本 JSON 链里认出加载器。顺序：标记文件 → 库坐标 → `mainClass`。
fn installed_loader(chain: &[Value], root_path: &Path, version_id: &str) -> Option<InstalledLoader> {
    let dir = root_path.join("versions").join(version_id);
    for (kind, marker) in [
        (LoaderKind::Fabric, ".fabric"),
        (LoaderKind::NeoForge, ".neoforge"),
        (LoaderKind::Forge, ".forge"),
    ] {
        let path = dir.join(marker).join("loader.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            return Some(InstalledLoader {
                kind,
                version: value
                    .get("loader")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }
    }

    let mut kind = None;
    let mut version = None;
    for value in chain {
        if let Some(main) = value.get("mainClass").and_then(|v| v.as_str()) {
            if main.contains("fabricmc") {
                kind = Some(LoaderKind::Fabric);
            } else if main.contains("neoforged") {
                kind = Some(LoaderKind::NeoForge);
            } else if main.contains("minecraftforge") {
                kind = Some(LoaderKind::Forge);
            }
        }
        for lib in value
            .get("libraries")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let Some(name) = lib.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            let pair = if lower.starts_with("net.fabricmc:fabric-loader") || lower.starts_with("org.quiltmc:quilt-loader") {
                Some((LoaderKind::Fabric, 2))
            } else if lower.starts_with("net.neoforged:neoforge") {
                Some((LoaderKind::NeoForge, 2))
            } else if lower.starts_with("net.minecraftforge:forge") {
                Some((LoaderKind::Forge, 2))
            } else {
                None
            };
            if let Some((k, idx)) = pair {
                kind.get_or_insert(k);
                if version.is_none() {
                    version = name.split(':').nth(idx).map(str::to_string);
                }
            }
        }
    }
    kind.map(|kind| InstalledLoader { kind, version })
}

/// 这个版本建立在哪个原版上：自包含布局看 `kaleidBaseGameVersion`，
/// 老布局看 `inheritsFrom`，再不行就用版本里的资源索引 id 兜底。
fn game_version_of(chain: &[Value], version_id: &str) -> Option<String> {
    crate::versions::base_game_version(chain, version_id)
}

fn dir_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok().map(|m| m.len()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_fabric_from_marker_and_libraries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("versions").join("26.3-Fabric");
        std::fs::create_dir_all(dir.join(".fabric")).unwrap();
        std::fs::write(
            dir.join(".fabric").join("loader.json"),
            json!({"kind": "fabric", "loader": "0.20.0"}).to_string(),
        )
        .unwrap();

        let found = installed_loader(&[json!({"id": "26.3-Fabric"})], root, "26.3-Fabric")
            .expect("marker should be read");
        assert_eq!(found.kind, LoaderKind::Fabric);
        assert_eq!(found.version.as_deref(), Some("0.20.0"));
    }

    #[test]
    fn detects_forge_from_main_class_without_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = vec![json!({
            "id": "1.20.1-forge",
            "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
            "libraries": [
                {"name": "net.minecraftforge:forge:1.20.1-47.2.0"},
                {"name": "net.minecraft:client:1.20.1"}
            ]
        })];
        let found = installed_loader(&chain, tmp.path(), "1.20.1-forge").expect("should detect");
        assert_eq!(found.kind, LoaderKind::Forge);
        assert_eq!(found.version.as_deref(), Some("1.20.1-47.2.0"));
    }

    #[test]
    fn vanilla_has_no_loader() {
        let tmp = tempfile::tempdir().unwrap();
        let chain = vec![json!({"id": "1.20.1", "mainClass": "net.minecraft.client.main.Main"})];
        assert!(installed_loader(&chain, tmp.path(), "1.20.1").is_none());
    }
}
