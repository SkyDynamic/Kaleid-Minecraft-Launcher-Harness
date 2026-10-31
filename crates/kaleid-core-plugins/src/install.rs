//! 安装编排：原版 / Fabric / Forge / NeoForge / Fabric API，以及自定义版本名。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use walkdir::WalkDir;

use kaleid_api::{
    ApiError, ApiResult, FabricApiInfo, InstallLoader, InstallQueued, InstallRequest, LoaderKind,
    LoaderVersionTimes, LoaderVersions, RemoteVersions, Root, RootRegistryService,
    SettingsServiceApi, VersionNameCheck, VersionStoreService,
};

use crate::downloads::{DownloadItem, DownloadManager, Job};
use crate::loaders;
use crate::remote::RemoteClient;
use crate::roots::RootRegistry;
use crate::settings::Settings;
use crate::versions::VersionStore;

#[derive(Clone)]
pub struct InstallService {
    settings: Arc<Settings>,
    versions: Arc<VersionStore>,
    roots: Arc<RootRegistry>,
    downloads: Arc<DownloadManager>,
    data_root: PathBuf,
    remote: Arc<RemoteClient>,
    reservations: Arc<Mutex<HashSet<String>>>,
}

impl InstallService {
    pub fn new(
        settings: Arc<Settings>,
        versions: Arc<VersionStore>,
        roots: Arc<RootRegistry>,
        downloads: Arc<DownloadManager>,
        data_root: &Path,
    ) -> Self {
        InstallService {
            settings,
            versions,
            roots,
            downloads,
            data_root: data_root.to_path_buf(),
            remote: Arc::new(RemoteClient::new()),
            reservations: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn remote_versions(&self, force: bool) -> ApiResult<RemoteVersions> {
        self.remote.versions(force)
    }

    pub fn loader_versions(&self, game: &str, loader: &str) -> ApiResult<LoaderVersions> {
        match loader {
            "fabric" => loaders::fabric_loaders(&self.remote, game),
            "forge" => loaders::forge_loaders(&self.remote, game),
            "neoforge" => loaders::neoforge_loaders(&self.remote, game),
            _ => Err(ApiError::invalid("加载器只能是 fabric / forge / neoforge")),
        }
    }

    pub fn loader_times(
        &self,
        game: &str,
        loader: &str,
        versions: &[String],
    ) -> ApiResult<LoaderVersionTimes> {
        loaders::loader_times(&self.remote, game, loader, versions)
    }

    pub fn fabric_api_latest(&self, game: &str) -> ApiResult<Option<FabricApiInfo>> {
        loaders::fabric_api_latest(&self.remote, game)
    }

    pub fn check_name(&self, root_id: &str, name: &str) -> ApiResult<VersionNameCheck> {
        let root = self.roots.get(root_id)?;
        Ok(match self.validate_and_check(&root, name) {
            Ok(()) => VersionNameCheck {
                ok: true,
                error: None,
            },
            Err(e) => VersionNameCheck {
                ok: false,
                error: Some(e),
            },
        })
    }

    pub fn enqueue(&self, root: Root, req: InstallRequest) -> ApiResult<InstallQueued> {
        validate_version_name(&req.version_name).map_err(ApiError::invalid)?;
        self.validate_and_check(&root, &req.version_name)
            .map_err(ApiError::invalid)?;
        self.check_conflict(&root, &req.game_version, req.loader_kind())?;
        let key = format!(
            "{}::{}",
            root.id.to_lowercase(),
            req.version_name.to_lowercase()
        );
        {
            let mut reservations = self.reservations.lock().unwrap();
            if !reservations.insert(key.clone()) {
                return Err(ApiError::invalid("该版本名称与现有文件夹重名"));
            }
        }

        let job = Job::new(
            uuid::Uuid::new_v4().to_string(),
            format!("安装 {}", req.version_name),
            req.version_name.clone(),
            root.id.clone(),
            req.game_version.clone(),
            req.loader_kind(),
            req.loader_version().map(|s| s.to_string()),
            self.settings
                .global()
                .map(|g| g.download_source)
                .unwrap_or_else(|_| kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_FIRST.into()),
        );
        self.downloads.register(job.clone());

        let this = self.clone();
        let job_for_thread = job.clone();
        let key_for_thread = key.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("kaleid-install".into())
            .spawn(move || {
                if let Err(e) = this.plan_and_run(&job_for_thread, &root, &req) {
                    job_for_thread.fail(e.to_string());
                }
                this.reservations.lock().unwrap().remove(&key_for_thread);
            })
        {
            self.reservations.lock().unwrap().remove(&key);
            return Err(ApiError::io(e.to_string()));
        }

        Ok(InstallQueued {
            job_id: job.id.clone(),
        })
    }

    /// 更新一个已装版本的加载器：只重铺加载器那一层，原版文件原地不动。
    ///
    /// 复用安装流程的三段（规划 → 下载 → 收尾），所以目录布局和全新安装完全一致；
    /// 差别只有"不重新下载客户端/资源"，以及合并时会把同名 artifact 的旧版本顶掉。
    pub fn update_loader(
        &self,
        root: Root,
        version_id: &str,
        loader: LoaderKind,
        loader_version: &str,
    ) -> ApiResult<InstallQueued> {
        let dir = loader_version_dir(&root, version_id);
        if !dir.is_dir() {
            return Err(ApiError::not_found(format!("版本 {version_id} 不存在")));
        }
        if matches!(loader, LoaderKind::Vanilla | LoaderKind::Unrecognized) {
            return Err(ApiError::invalid("原版版本没有加载器可以更新"));
        }
        let base = read_version_json(&dir, version_id)?;
        let game = base
            .get("kaleidBaseGameVersion")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                base.get("inheritsFrom")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .or_else(|| {
                base.get("assetIndex")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                ApiError::Format(format!(
                    "读不出 {version_id} 对应的原版游戏版本，无法确定要装的加载器版本"
                ))
            })?;

        let job = Job::new(
            uuid::Uuid::new_v4().to_string(),
            format!("更新 {version_id} 的{}", loader.label_zh()),
            version_id.to_string(),
            root.id.clone(),
            game.clone(),
            loader,
            Some(loader_version.to_string()),
            self.settings
                .global()
                .map(|g| g.download_source)
                .unwrap_or_else(|_| kaleid_api::DOWNLOAD_SOURCE_OFFICIAL_FIRST.into()),
        );
        self.downloads.register(job.clone());

        let this = self.clone();
        let job_for_thread = job.clone();
        let version_for_thread = version_id.to_string();
        let loader_for_thread = loader_version.to_string();
        std::thread::Builder::new()
            .name("kaleid-loader-update".into())
            .spawn(move || {
                let result = (|| -> ApiResult<()> {
                    let staging = this
                        .data_root
                        .join("staging")
                        .join(format!("{}-{}", loader.slug(), job_for_thread.id));
                    job_for_thread.start_phase("prepare");
                    job_for_thread.set_current(format!("解析 {} 加载器信息…", loader.label_zh()));
                    let mut items = match loader {
                        LoaderKind::Fabric => this.fabric_items(
                            &root,
                            &version_for_thread,
                            &game,
                            &loader_for_thread,
                        )?,
                        LoaderKind::Forge => {
                            this.forge_items(&job_for_thread, &game, &loader_for_thread)?
                        }
                        LoaderKind::NeoForge => {
                            this.neoforge_items(&job_for_thread, &game, &loader_for_thread)?
                        }
                        _ => return Err(ApiError::invalid("这个版本没有可更新的加载器")),
                    };
                    items.retain(|i| !(i.dest.exists() && i.size > 0));
                    job_for_thread.complete_phase("prepare");
                    for item in &items {
                        job_for_thread.add_total(&item.phase, item.size, 1);
                    }
                    job_for_thread.set_status(kaleid_api::DownloadStatus::Queued, "准备下载");
                    this.downloads.run_batch(job_for_thread.clone(), items)?;
                    if matches!(loader, LoaderKind::Forge | LoaderKind::NeoForge) {
                        job_for_thread.start_phase("installer");
                        job_for_thread
                            .set_current(format!("运行 {} 官方安装器…", loader.label_zh()));
                        this.finish_modded_installer(
                            &job_for_thread,
                            &root,
                            &version_for_thread,
                            &game,
                            loader.label_zh(),
                            &staging,
                        )?;
                        job_for_thread.complete_phase("installer");
                    }
                    job_for_thread.complete();
                    Ok(())
                })();
                if let Err(e) = result {
                    job_for_thread.fail(e.to_string());
                }
            })
            .map_err(|e| ApiError::io(e.to_string()))?;

        Ok(InstallQueued {
            job_id: job.id.clone(),
        })
    }

    fn validate_and_check(&self, root: &Root, name: &str) -> Result<(), String> {
        validate_version_name(name)?;
        let dir = Path::new(&root.path).join("versions");
        if dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let existing = entry.file_name().to_string_lossy().to_lowercase();
                        if existing == name.to_lowercase() {
                            return Err("该版本名称与现有文件夹重名".into());
                        }
                    }
                }
            }
        }
        let key = format!("{}::{}", root.id.to_lowercase(), name.to_lowercase());
        if self.reservations.lock().unwrap().contains(&key) {
            return Err("该版本名称与现有文件夹重名".into());
        }
        Ok(())
    }

    fn check_conflict(&self, root: &Root, game: &str, requested: LoaderKind) -> ApiResult<()> {
        if requested == LoaderKind::Vanilla {
            return Ok(());
        }
        let list = self.versions.scan(Path::new(&root.path))?;
        for v in list {
            if v.inherits_from.as_deref() != Some(game) {
                continue;
            }
            if v.loader == LoaderKind::Vanilla {
                continue;
            }
            if v.loader != requested {
                return Err(ApiError::invalid(format!(
                    "该游戏版本已安装 {} {}，不能同时安装 {}；请换一个游戏版本或先移除旧版本。",
                    v.loader.label_zh(),
                    v.loader_version.as_deref().unwrap_or(""),
                    requested.label_zh()
                )));
            }
        }
        Ok(())
    }

    fn plan_and_run(&self, job: &Arc<Job>, root: &Root, req: &InstallRequest) -> ApiResult<()> {
        job.start_phase("prepare");
        job.set_current("解析版本信息…");
        let mut items = Vec::new();
        match &req.loader {
            InstallLoader::Vanilla => {
                items.extend(self.vanilla_items(root, &req.version_name, &req.game_version)?);
            }
            InstallLoader::Fabric { version } => {
                // 自包含布局：原版与加载器全部落在 `versions/<版本名>/` 里，
                // 不再额外造一个 `versions/<原版 GameVersion>/` 当 inheritsFrom 的父级。
                items.extend(self.vanilla_items(root, &req.version_name, &req.game_version)?);
                items.extend(self.fabric_items(
                    root,
                    &req.version_name,
                    &req.game_version,
                    version,
                )?);
                if req.install_fabric_api {
                    if let Some(api) = loaders::fabric_api_latest(&self.remote, &req.game_version)?
                    {
                        let global = self.settings.global()?;
                        let isolated =
                            kaleid_api::resolve_isolation(root.isolation, global.isolation_default);
                        let mods = root.game_dir(&req.version_name, isolated).join("mods");
                        let has_other = std::fs::read_dir(&mods)
                            .ok()
                            .into_iter()
                            .flatten()
                            .flatten()
                            .any(|e| {
                                let n = e.file_name().to_string_lossy().to_string();
                                n.starts_with("fabric-api-")
                                    && n.ends_with(".jar")
                                    && n != api.file_name
                            });
                        if !has_other {
                            items.push(DownloadItem {
                                label: format!("Fabric API {}", api.version),
                                phase: "fabric-api".into(),
                                official_url: Some(api.download_url.clone()),
                                // Fabric API 只落在 Modrinth 的 CDN 上，而那个域名在部分网络里
                                // 连不上（典型是 os error 10060）。MCIM 镜像能按前缀拿到同一
                                // 份文件，所以这里补一条镜像地址；到底先用哪个仍由下载源策略
                                // 决定（`ordered_urls`），官方优先时官方失败会立刻换过来。
                                mirror_url: kaleid_api::mirror_url_for(&api.download_url),
                                fallback_url: None,
                                dest: mods.join(&api.file_name),
                                size: api.size,
                                sha1: api.sha1.clone(),
                            });
                        }
                    }
                }
            }
            InstallLoader::Forge { version } => {
                items.extend(self.vanilla_items(root, &req.version_name, &req.game_version)?);
                items.extend(self.forge_items(job, &req.game_version, version)?);
            }
            InstallLoader::Neoforge { version } => {
                items.extend(self.vanilla_items(root, &req.version_name, &req.game_version)?);
                items.extend(self.neoforge_items(job, &req.game_version, version)?);
            }
        }

        job.complete_phase("prepare");
        for item in &items {
            job.add_total(&item.phase, item.size, 1);
        }
        job.set_status(kaleid_api::DownloadStatus::Queued, "准备下载");
        self.downloads.run_batch(job.clone(), items)?;

        let installer = match &req.loader {
            InstallLoader::Forge { .. } => Some(("Forge", "forge")),
            InstallLoader::Neoforge { .. } => Some(("NeoForge", "neoforge")),
            _ => None,
        };
        if let Some((label, slug)) = installer {
            job.start_phase("installer");
            job.set_current(format!("运行 {label} 官方安装器…"));
            let staging = self
                .data_root
                .join("staging")
                .join(format!("{slug}-{}", job.id));
            self.finish_modded_installer(
                job,
                root,
                &req.version_name,
                &req.game_version,
                label,
                &staging,
            )?;
            job.complete_phase("installer");
        }
        job.complete();
        Ok(())
    }

    fn vanilla_items(&self, root: &Root, name: &str, game: &str) -> ApiResult<Vec<DownloadItem>> {
        let manifest = self.remote.versions(false)?;
        let rv = manifest
            .versions
            .iter()
            .find(|v| v.id == game)
            .ok_or_else(|| ApiError::not_found(format!("版本清单中没有 {game}")))?;
        let official_json = rv.url.clone();
        let mirror_json = format!(
            "https://bmclapi2.bangbang93.com/version/{}/json",
            loaders::url_encode(game)
        );
        let value = self
            .remote
            .get_json(&official_json)
            .or_else(|_| self.remote.get_json(&mirror_json))?;
        let mut id_value = value.clone();
        id_value["id"] = json!(name);
        write_version_json(root, name, &id_value)?;

        let root_path = Path::new(&root.path);
        let mut items = Vec::new();
        let client = value
            .get("downloads")
            .and_then(|d| d.get("client"))
            .ok_or_else(|| ApiError::Format(format!("{game} 缺少客户端下载信息")))?;
        let client_url = client
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ApiError::Format("客户端下载信息缺少 url".into()))?;
        items.push(DownloadItem {
            label: format!("{game} 客户端"),
            phase: "client".into(),
            official_url: Some(client_url.to_string()),
            mirror_url: Some(format!(
                "https://bmclapi2.bangbang93.com/version/{}/client",
                loaders::url_encode(game)
            )),
            fallback_url: None,
            dest: root_path
                .join("versions")
                .join(name)
                .join(format!("{name}.jar")),
            size: client.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
            sha1: client
                .get("sha1")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        });

        if let Some(libs) = value.get("libraries").and_then(|v| v.as_array()) {
            for lib in libs {
                let allowed = lib
                    .get("rules")
                    .and_then(|r| r.as_array())
                    .map(|r| rules_allow(r))
                    .unwrap_or(true);
                if !allowed {
                    continue;
                }
                let Some(artifact) = lib.get("downloads").and_then(|d| d.get("artifact")) else {
                    continue;
                };
                let Some(path) = artifact.get("path").and_then(|v| v.as_str()) else {
                    continue;
                };
                let url = artifact
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("https://libraries.minecraft.net/{path}"));
                items.push(DownloadItem {
                    label: format!("依赖库 {path}"),
                    phase: "libraries".into(),
                    official_url: Some(url),
                    mirror_url: Some(format!("https://bmclapi2.bangbang93.com/maven/{path}")),
                    fallback_url: None,
                    dest: root_path.join("libraries").join(path),
                    size: artifact.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    sha1: artifact
                        .get("sha1")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                });
            }
        }

        if let Some(index) = value.get("assetIndex") {
            let index_id = index
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::Format("assetIndex 缺少 id".into()))?;
            let index_url = index
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::Format("assetIndex 缺少 url".into()))?;
            let mirror_index = format!(
                "https://bmclapi2.bangbang93.com/version/{}/assetIndex",
                loaders::url_encode(game)
            );
            items.push(DownloadItem {
                label: format!("资源索引 {index_id}"),
                phase: "assets".into(),
                official_url: Some(index_url.to_string()),
                mirror_url: Some(mirror_index.clone()),
                fallback_url: None,
                dest: root_path
                    .join("assets/indexes")
                    .join(format!("{index_id}.json")),
                size: index.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                sha1: index
                    .get("sha1")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            });
            let index_value = self
                .remote
                .get_json(index_url)
                .or_else(|_| self.remote.get_json(&mirror_index))?;
            let mut seen = HashSet::new();
            if let Some(objects) = index_value.get("objects").and_then(|v| v.as_object()) {
                for obj in objects.values() {
                    let Some(hash) = obj.get("hash").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    if !seen.insert(hash.to_string()) {
                        continue;
                    }
                    let prefix = &hash[..2.min(hash.len())];
                    items.push(DownloadItem {
                        label: format!("资源 {hash}"),
                        phase: "assets".into(),
                        official_url: Some(format!(
                            "https://resources.download.minecraft.net/{prefix}/{hash}"
                        )),
                        mirror_url: Some(format!(
                            "https://bmclapi2.bangbang93.com/assets/{prefix}/{hash}"
                        )),
                        fallback_url: None,
                        dest: root_path.join("assets/objects").join(prefix).join(hash),
                        size: obj.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                        sha1: Some(hash.to_string()),
                    });
                }
            }
        }

        if let Some(file) = value
            .get("logging")
            .and_then(|l| l.get("client"))
            .and_then(|c| c.get("file"))
        {
            if let (Some(id), Some(url)) = (
                file.get("id").and_then(|v| v.as_str()),
                file.get("url").and_then(|v| v.as_str()),
            ) {
                items.push(DownloadItem {
                    label: format!("日志配置 {id}"),
                    phase: "assets".into(),
                    official_url: Some(url.to_string()),
                    mirror_url: None,
                    fallback_url: None,
                    dest: root_path.join("assets/log_configs").join(id),
                    size: file.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    sha1: file
                        .get("sha1")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                });
            }
        }
        Ok(items)
    }

    fn fabric_items(
        &self,
        root: &Root,
        name: &str,
        game: &str,
        loader: &str,
    ) -> ApiResult<Vec<DownloadItem>> {
        let dir = loader_version_dir(root, name);
        // 基座就是上一步 `vanilla_items` 刚写好的纯净原版 JSON：它已经是
        // `versions/<版本名>/<版本名>.json`，所以这里只做「合并进同一个文件」，
        // 不再造父子两层（PCL 那种 `版本名/版本名.json` 单文件布局）。
        let base = read_version_json(&dir, name)?;
        let overlay = loaders::fabric_profile(&self.remote, game, loader)?;
        let merged = merge_profile(&base, &overlay, name, game);
        write_version_json(root, name, &merged)?;
        write_loader_marker(&dir, "fabric", loader, game)?;
        let mut items = Vec::new();
        if let Some(libs) = overlay.get("libraries").and_then(|v| v.as_array()) {
            for lib in libs {
                let Some(coordinate) = lib.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(path) = loaders::maven_path(coordinate) else {
                    continue;
                };
                let base_url = lib
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("https://maven.fabricmc.net/");
                let url = lib
                    .get("downloads")
                    .and_then(|d| d.get("artifact"))
                    .and_then(|a| a.get("url"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{}/{}", base_url.trim_end_matches('/'), path));
                let size = lib
                    .get("size")
                    .and_then(|v| v.as_u64())
                    .or_else(|| {
                        lib.get("downloads")
                            .and_then(|d| d.get("artifact"))
                            .and_then(|a| a.get("size"))
                            .and_then(|v| v.as_u64())
                    })
                    .unwrap_or(0);
                let sha1 = lib
                    .get("sha1")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        lib.get("downloads")
                            .and_then(|d| d.get("artifact"))
                            .and_then(|a| a.get("sha1"))
                            .and_then(|v| v.as_str())
                    })
                    .map(|s| s.to_string());
                items.push(DownloadItem {
                    label: format!("Fabric {coordinate}"),
                    phase: "loader".into(),
                    official_url: Some(url),
                    // Fabric 的库在 `maven.fabricmc.net`，和 NeoForge 一样国内经常超时。
                    // BMCLAPI 的 `/maven/` 按同一条相对路径镜像这份文件。
                    mirror_url: Some(loaders::bmclapi_maven_url(&path)),
                    fallback_url: None,
                    // 加载器库也进版本目录：整个 `versions/<版本名>/` 可以直接拷走。
                    dest: dir.join("libraries").join(&path),
                    size,
                    sha1,
                });
            }
        }
        Ok(items)
    }

    fn forge_items(
        &self,
        job: &Arc<Job>,
        game: &str,
        version: &str,
    ) -> ApiResult<Vec<DownloadItem>> {
        let staging = self
            .data_root
            .join("staging")
            .join(format!("forge-{}", job.id));
        std::fs::create_dir_all(&staging)?;
        let installer = staging.join("installer.jar");
        let url = loaders::forge_installer_url(game, version);
        let sha1 = self
            .remote
            .get_text(&loaders::forge_installer_sha1_url(game, version))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(vec![DownloadItem {
            label: format!("Forge {version} 安装器"),
            phase: "installer".into(),
            official_url: Some(url),
            mirror_url: Some(loaders::forge_installer_mirror_url(game, version)),
            fallback_url: None,
            dest: installer,
            size: 0,
            sha1,
        }])
    }

    fn neoforge_items(
        &self,
        job: &Arc<Job>,
        game: &str,
        version: &str,
    ) -> ApiResult<Vec<DownloadItem>> {
        let staging = self
            .data_root
            .join("staging")
            .join(format!("neoforge-{}", job.id));
        std::fs::create_dir_all(&staging)?;
        let installer = staging.join("installer.jar");
        let url = loaders::neoforge_installer_url(version);
        let sha1 = self
            .remote
            .get_text(&loaders::neoforge_installer_sha1_url(version))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let _ = game;
        Ok(vec![DownloadItem {
            label: format!("NeoForge {version} 安装器"),
            phase: "installer".into(),
            official_url: Some(url),
            mirror_url: Some(loaders::neoforge_installer_mirror_url(version)),
            fallback_url: None,
            dest: installer,
            size: 0,
            sha1,
        }])
    }

    fn finish_modded_installer(
        &self,
        job: &Arc<Job>,
        root: &Root,
        name: &str,
        game: &str,
        label: &str,
        staging: &Path,
    ) -> ApiResult<()> {
        let base_dir = loader_version_dir(root, name);
        let staging_base = staging.join("versions").join(game);
        std::fs::create_dir_all(&staging_base)?;
        for ext in ["json", "jar"] {
            // 安装器只认 `<game>.json` / `<game>.jar` 这一对名字，所以先在 staging 里
            // 用原名落一份（硬链接省一次 GB 级拷贝），装完再把结果并回版本目录。
            let src = base_dir.join(format!("{name}.{ext}"));
            let dst = staging_base.join(format!("{game}.{ext}"));
            if src.is_file() {
                if std::fs::hard_link(&src, &dst).is_err() {
                    std::fs::copy(&src, &dst)?;
                }
            }
        }
        std::fs::write(
            staging.join("launcher_profiles.json"),
            br#"{"profiles":{}}"#,
        )?;
        // PCL2 的做法：先按 install_profile.json / version.json 把库从 BMCLAPI
        // 下进 staging/libraries，再跑官方安装器。安装器看到校验通过的文件就
        // 不再去 maven.neoforged.net / maven.minecraftforge.net（国内经常超时）。
        self.prefetch_installer_libraries(job, staging, label)?;
        let java = resolve_java(&self.settings.global()?);
        let log = staging.join("installer.log");
        let out = std::fs::File::create(&log)?;
        let err = out.try_clone()?;
        let mut child = std::process::Command::new(&java)
            .arg("-jar")
            .arg(staging.join("installer.jar"))
            .arg("--installClient")
            .arg(staging)
            .current_dir(staging)
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            .spawn()
            .map_err(|e| ApiError::io(format!("无法启动 Java（{java}）：{e}")))?;
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().map_err(ApiError::io)? {
                if !status.success() {
                    let tail = tail_lines(&log, 20);
                    return Err(ApiError::Network(format!(
                        "{label} 安装器失败（{status}）。依赖已尽量从 BMCLAPI 补过，下面是安装器自己的输出：\n{tail}"
                    )));
                }
                break;
            }
            if start.elapsed() > Duration::from_secs(900) {
                let _ = child.kill();
                return Err(ApiError::Network(format!("{label} 安装器超时")));
            }
            std::thread::sleep(Duration::from_millis(250));
        }

        let versions_dir = staging.join("versions");
        let mut produced: Option<PathBuf> = None;
        if let Ok(entries) = std::fs::read_dir(&versions_dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy() == game {
                    continue;
                }
                if let Ok(inner) = std::fs::read_dir(entry.path()) {
                    for file in inner.flatten() {
                        if file.file_name().to_string_lossy().ends_with(".json") {
                            produced = Some(entry.path());
                            break;
                        }
                    }
                }
                if produced.is_some() {
                    break;
                }
            }
        }
        let produced = produced.ok_or_else(|| ApiError::Format("安装器没有生成版本目录".into()))?;
        let dir = loader_version_dir(root, name);
        std::fs::create_dir_all(&dir)?;

        // 安装器产出的 JSON 是带 `inheritsFrom` 的「增量」版本，这里把它并进
        // 版本目录里那份完整的原版 JSON，得到一个自包含的单文件版本。
        let produced_json = std::fs::read_dir(&produced)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .ok_or_else(|| ApiError::Format("安装器版本目录缺少 JSON".into()))?;
        let overlay: Value = serde_json::from_str(&std::fs::read_to_string(&produced_json)?)?;
        let base = read_version_json(&dir, name)?;
        let merged = merge_profile(&base, &overlay, name, game);
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_string_pretty(&merged)?,
        )?;

        // 安装器把加载器库下到了 staging；挪进版本目录自己的 libraries/。
        let staging_libs = staging.join("libraries");
        let version_libs = dir.join("libraries");
        if staging_libs.is_dir() {
            for entry in WalkDir::new(&staging_libs).into_iter().flatten() {
                if entry.file_type().is_file() {
                    let rel = entry.path().strip_prefix(&staging_libs).unwrap();
                    let target = version_libs.join(rel);
                    if let Some(p) = target.parent() {
                        std::fs::create_dir_all(p)?;
                    }
                    if !target.exists() {
                        let _ = std::fs::rename(entry.path(), &target)
                            .or_else(|_| std::fs::copy(entry.path(), &target).map(|_| ()));
                    }
                }
            }
        }
        let kind = if label.eq_ignore_ascii_case("neoforge") {
            "neoforge"
        } else {
            "forge"
        };
        let loader_version = overlay
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        write_loader_marker(&dir, kind, &loader_version, game)?;
        let _ = std::fs::remove_dir_all(&produced);
        let _ = std::fs::remove_dir_all(staging);
        Ok(())
    }

    /// 把安装器自己要下的库先经下载器（含 BMCLAPI 镜像）放进 `staging/libraries`。
    ///
    /// 官方安装器写死了 `maven.neoforged.net` / `maven.minecraftforge.net`，
    /// 连不上就整次失败（日志里是一串坐标 + `There was an error during installation`）。
    /// 它自己会跳过「文件已在且 SHA-1 对得上」的库，所以先补齐就行，不必改安装器。
    fn prefetch_installer_libraries(
        &self,
        job: &Arc<Job>,
        staging: &Path,
        label: &str,
    ) -> ApiResult<()> {
        let installer = staging.join("installer.jar");
        let profile_bytes = crate::jar::read_entry_bytes(&installer, "install_profile.json", 2 * 1024 * 1024)
            .ok_or_else(|| {
                ApiError::Format(format!(
                    "{label} 安装器里没有 install_profile.json，无法按镜像补库"
                ))
            })?;
        let profile: Value = serde_json::from_slice(&profile_bytes)?;
        let version = crate::jar::read_entry_bytes(&installer, "version.json", 2 * 1024 * 1024)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        let libs_dir = staging.join("libraries");
        let mut items = installer_library_items(&profile, &version, &libs_dir);
        items.retain(|item| !file_already_good(&item.dest, item.sha1.as_deref(), item.size));
        if items.is_empty() {
            return Ok(());
        }
        job.set_current(format!(
            "从镜像补全 {label} 安装器依赖（{} 个）…",
            items.len()
        ));
        for item in &items {
            job.add_total("installer", item.size, 1);
        }
        self.downloads.run_batch(job.clone(), items)
    }
}

/// 写标记目录时用的加载器英文标识（`.fabric` / `.forge` / `.neoforge`）。
pub fn loader_marker_kind(kind: LoaderKind) -> Option<&'static str> {
    match kind {
        LoaderKind::Fabric => Some("fabric"),
        LoaderKind::Forge => Some("forge"),
        LoaderKind::NeoForge => Some("neoforge"),
        _ => None,
    }
}

/// Maven 坐标 → `libraries/` 下的相对路径。
///
/// `group:artifact:version[:classifier]` →
/// `group/artifact/version/artifact-version[-classifier].jar`
///
/// 这是官方启动器自己的算法（`Library.getName()` + `getPath()`）。我们本来只认
/// JSON 里的 `downloads.artifact.path`，但很多第三方启动器写出来的加载器 profile
/// 只有坐标（例如 `{"name": "net.fabricmc:fabric-loader:0.18.3", "url": "..."}`），
/// 结果依赖被静默丢掉——游戏直接起不来。坐标都推不出路径时才放弃。
pub(crate) fn library_path_from_name(name: &str) -> Option<String> {
    // 去掉可能存在的 `@zip` 扩展名后缀
    let (name, ext) = match name.split_once('@') {
        Some((head, ext)) => (head, ext),
        None => (name, "jar"),
    };
    let parts: Vec<&str> = name.split(':').collect();
    let (group, artifact, version, classifier) = match parts.as_slice() {
        [group, artifact, version] => (*group, *artifact, *version, None),
        [group, artifact, version, classifier] => (*group, *artifact, *version, Some(*classifier)),
        _ => return None,
    };
    if group.is_empty() || artifact.is_empty() || version.is_empty() {
        return None;
    }
    let file = match classifier {
        Some(classifier) if !classifier.is_empty() => {
            format!("{artifact}-{version}-{classifier}.{ext}")
        }
        _ => format!("{artifact}-{version}.{ext}"),
    };
    Some(format!(
        "{}/{artifact}/{version}/{file}",
        group.replace('.', "/")
    ))
}

/// 从安装器的 `install_profile.json` 和 `version.json` 抽出要预下载的库。
///
/// 两份都要：profile 里有安装器自己用的（`installertools`、`*-universal`），
/// version.json 里有运行时库。按目标路径去重。
fn installer_library_items(profile: &Value, version: &Value, libraries_dir: &Path) -> Vec<DownloadItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for doc in [profile, version] {
        let Some(libs) = doc.get("libraries").and_then(|v| v.as_array()) else {
            continue;
        };
        for lib in libs {
            let Some(item) = installer_library_item(lib, libraries_dir) else {
                continue;
            };
            if seen.insert(item.dest.clone()) {
                items.push(item);
            }
        }
    }
    items
}

fn installer_library_item(lib: &Value, libraries_dir: &Path) -> Option<DownloadItem> {
    let path = library_relative_path(lib)?;
    let artifact = lib.get("downloads").and_then(|d| d.get("artifact"));
    let official = artifact
        .and_then(|a| a.get("url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            let base = lib.get("url").and_then(|v| v.as_str())?;
            Some(format!(
                "{}/{}",
                base.trim_end_matches('/'),
                path.trim_start_matches('/')
            ))
        });
    let name = lib
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(path.as_str());
    Some(DownloadItem {
        label: format!("安装器依赖 {name}"),
        phase: "installer".into(),
        official_url: official,
        mirror_url: Some(loaders::bmclapi_maven_url(&path)),
        fallback_url: None,
        dest: libraries_dir.join(&path),
        size: artifact
            .and_then(|a| a.get("size"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        sha1: artifact
            .and_then(|a| a.get("sha1"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

/// 文件已在且 SHA-1（或至少大小）对得上，安装器会直接跳过，不必再下。
fn file_already_good(path: &Path, sha1: Option<&str>, size: u64) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    if bytes.is_empty() {
        return false;
    }
    if size > 0 && bytes.len() as u64 != size {
        return false;
    }
    match sha1 {
        Some(expected) => hex::encode(Sha1::digest(&bytes)).eq_ignore_ascii_case(expected),
        None => true,
    }
}

/// 一个库在 `libraries/` 下的相对路径：优先 JSON 给好的，退回到 Maven 坐标推导。
///
/// 拼 classpath 与"缺不缺文件"的检查都走这一条，口径必须一致——否则会出现
/// "能启动但界面报缺 30 个文件"或者反过来。
pub(crate) fn library_relative_path(lib: &Value) -> Option<String> {
    lib.get("downloads")
        .and_then(|d| d.get("artifact"))
        .and_then(|a| a.get("path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            lib.get("name")
                .and_then(|v| v.as_str())
                .and_then(library_path_from_name)
        })
}

/// Maven 坐标的「身份键」：`group:artifact[:classifier]`，忽略版本号。
///
/// 拼 classpath 时也用它去重（见 `launch::build_plan`）：同一个库的两个版本同时
/// 出现在 classpath 上时 Fabric 会直接拒绝启动
/// （`duplicate ASM classes found on classpath`），Forge 侧则是类加载顺序问题。
pub(crate) fn artifact_key(name: &str) -> String {
    let parts: Vec<&str> = name.split(':').collect();
    match parts.len() {
        0 | 1 => name.to_string(),
        4 => format!("{}:{}:{}", parts[0], parts[1], parts[3]),
        _ => format!("{}:{}", parts[0], parts[1]),
    }
}

fn tail_lines(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn write_version_json(root: &Root, name: &str, value: &Value) -> ApiResult<()> {
    let dir = loader_version_dir(root, name);
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(dir.join(format!("{name}.json")), text)?;
    Ok(())
}

/// 自包含布局的版本目录：`<游戏目录>/versions/<版本名>/`。
///
/// 装加载器时把 JSON、客户端 jar、加载器库、`.fabric` 之类的标记全放进来，
/// 整个目录可以直接拷到别的游戏目录里用，也不需要 `inheritsFrom` 去指父版本。
pub fn loader_version_dir(root: &Root, name: &str) -> PathBuf {
    Path::new(&root.path).join("versions").join(name)
}

fn read_version_json(dir: &Path, name: &str) -> ApiResult<Value> {
    let path = dir.join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ApiError::not_found(format!("{}（{e}）", path.display())))?;
    serde_json::from_str(&text).map_err(|e| ApiError::Format(format!("{}: {e}", path.display())))
}

/// 把「基座原版 JSON」和「加载器 profile」压成**一个**版本 JSON。
///
/// 合并规则刻意简单：基座提供 `downloads` / `assetIndex` / `javaVersion` 这些
/// 安装器必读的字段，加载器提供 `mainClass` 与自己那段 `arguments`，两者之后
/// 不再有 `inheritsFrom` —— 这样版本目录自包含，扫描/启动都不必再回溯父版本。
pub fn merge_profile(base: &Value, overlay: &Value, name: &str, game: &str) -> Value {
    let mut merged = base.clone();
    let Some(obj) = merged.as_object_mut() else {
        return base.clone();
    };
    obj.insert("id".into(), json!(name));
    // 自定义键：加载器装好之后仍然能知道它建立在哪个原版上（启动参数、模组筛选
    // 都要用），官方 JSON 没有这个字段，所以自己加一个。
    obj.insert("kaleidBaseGameVersion".into(), json!(game));
    obj.remove("inheritsFrom");

    for (key, value) in overlay.as_object().into_iter().flatten() {
        match key.as_str() {
            // 父级 id / 继承关系在合并后没有意义。
            "inheritsFrom" | "id" => {}
            "libraries" => {
                let mut libs = obj
                    .get("libraries")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                // 同 group:artifact（含 classifier）视为同一个库：更新加载器时
                // 新版要顶掉旧版，否则两代加载器库会一起留在 classpath 上。
                let overridden: HashSet<String> = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.get("name").and_then(|v| v.as_str()))
                    .map(artifact_key)
                    .collect();
                libs.retain(|l| {
                    l.get("name")
                        .and_then(|v| v.as_str())
                        .map(|n| !overridden.contains(&artifact_key(n)))
                        .unwrap_or(true)
                });
                let mut seen: HashSet<String> = libs
                    .iter()
                    .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .collect();
                for lib in value.as_array().into_iter().flatten() {
                    let key = lib
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if !key.is_empty() && !seen.insert(key) {
                        continue;
                    }
                    libs.push(lib.clone());
                }
                obj.insert("libraries".into(), Value::Array(libs));
            }
            "arguments" => {
                // 顺序是「基座在前、加载器在后」：加载器要覆盖的参数后写，
                // JVM 的 `-D` 同名时后者生效。
                let mut args = obj.get("arguments").cloned().unwrap_or_else(|| json!({}));
                if let Some(map) = args.as_object_mut() {
                    for slot in ["game", "jvm"] {
                        let Some(extra) = value.get(slot).and_then(|v| v.as_array()) else {
                            continue;
                        };
                        let mut list = map
                            .get(slot)
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        list.extend(extra.iter().cloned());
                        map.insert(slot.to_string(), Value::Array(list));
                    }
                }
                obj.insert("arguments".into(), args);
            }
            _ => {
                obj.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

/// 写下加载器标记目录（`.fabric` / `.forge` / `.neoforge`）。
///
/// 这些标记不是给启动器自己看的（启动器认 JSON），而是给「这个版本到底装了
/// 什么加载器、装的哪个版本」留一份不会被用户误改的现场记录：卸载/更新加载器、
/// 以及用户手动翻目录时都要靠它。PCL 也是同样做法。
pub fn write_loader_marker(dir: &Path, kind: &str, loader: &str, game: &str) -> ApiResult<()> {
    let marker = dir.join(format!(".{kind}"));
    std::fs::create_dir_all(&marker)?;
    let value = json!({
        "kind": kind,
        "loader": loader,
        "game": game,
        "installedAt": unix_now_secs(),
    });
    std::fs::write(
        marker.join("loader.json"),
        serde_json::to_string_pretty(&value)?,
    )?;
    Ok(())
}

/// 只用于标记文件的时间戳；不引第三方时间库，秒级就够。
fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn resolve_java(global: &kaleid_api::GlobalSettings) -> String {
    if let Some(path) = &global.java_path {
        let p = PathBuf::from(path);
        if p.is_file() {
            if p.file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case("javaw.exe"))
                .unwrap_or(false)
            {
                if let Some(parent) = p.parent() {
                    let java = parent.join("java.exe");
                    if java.is_file() {
                        return java.to_string_lossy().to_string();
                    }
                }
            }
            return p.to_string_lossy().to_string();
        }
    }
    "java".into()
}

pub fn validate_version_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("版本名称不能为空".into());
    }
    if name != name.trim() {
        return Err("名称首尾不能有空格".into());
    }
    if name == "." || name == ".." {
        return Err("版本名称不合法".into());
    }
    if name.ends_with('.') {
        return Err("文件名不能以小数点结尾".into());
    }
    if name.chars().count() > 64 {
        return Err("名称不能超过 64 个字符".into());
    }
    if name.chars().any(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control()
    }) {
        return Err("名称不能包含 < > : \" / \\ | ? *".into());
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        return Err("名称不能是 Windows 保留名（CON、PRN、AUX、NUL、COM1–9、LPT1–9）".into());
    }
    Ok(())
}

pub(crate) fn rules_allow(rules: &[Value]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if rule_matches(rule) {
            allowed = rule.get("action").and_then(|a| a.as_str()) == Some("allow");
        }
    }
    allowed
}

fn rule_matches(rule: &Value) -> bool {
    if let Some(os) = rule.get("os") {
        if let Some(name) = os.get("name").and_then(|v| v.as_str()) {
            let current = match std::env::consts::OS {
                "windows" => "windows",
                "macos" => "osx",
                other => other,
            };
            if current != name {
                return false;
            }
        }
        if let Some(arch) = os.get("arch").and_then(|v| v.as_str()) {
            if std::env::consts::ARCH != arch {
                return false;
            }
        }
    }
    if let Some(features) = rule.get("features").and_then(|v| v.as_object()) {
        for want in features.values() {
            if want.as_bool() == Some(true) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_windows_names() {
        assert!(validate_version_name("1.20.1-Fabric 0.19.2").is_ok());
        assert_eq!(
            validate_version_name("foo.").unwrap_err(),
            "文件名不能以小数点结尾"
        );
        assert!(validate_version_name("a/b").is_err());
        assert!(validate_version_name("CON").is_err());
        assert!(validate_version_name(" ").is_err());
    }

    /// 安装器依赖必须带 BMCLAPI 镜像，并且落到 staging/libraries 的 Maven 路径上。
    ///
    /// 这是「正常退出被当成崩溃」同类的安装失败：官方安装器自己去
    /// maven.neoforged.net，超时后只甩一串坐标。预下载清单要对得上那串坐标。
    #[test]
    fn installer_libraries_are_mirrored_onto_the_staging_libraries_dir() {
        let profile = json!({
            "libraries": [{
                "name": "org.jspecify:jspecify:1.0.1",
                "downloads": {"artifact": {
                    "path": "org/jspecify/jspecify/1.0.1/jspecify-1.0.1.jar",
                    "url": "https://maven.neoforged.net/releases/org/jspecify/jspecify/1.0.1/jspecify-1.0.1.jar",
                    "sha1": "abc",
                    "size": 12
                }}
            }, {
                "name": "net.neoforged:neoforge:26.3.0.99:universal",
                "downloads": {"artifact": {
                    "path": "net/neoforged/neoforge/26.3.0.99/neoforge-26.3.0.99-universal.jar",
                    "url": "https://maven.neoforged.net/releases/net/neoforged/neoforge/26.3.0.99/neoforge-26.3.0.99-universal.jar"
                }}
            }]
        });
        // version.json 里再列一次 jspecify：只能留一条，不能下两遍。
        let version = json!({
            "libraries": [{
                "name": "org.jspecify:jspecify:1.0.1",
                "downloads": {"artifact": {
                    "path": "org/jspecify/jspecify/1.0.1/jspecify-1.0.1.jar",
                    "url": "https://maven.neoforged.net/releases/org/jspecify/jspecify/1.0.1/jspecify-1.0.1.jar"
                }}
            }, {
                "name": "net.fabricmc:sponge-mixin:0.17.4+mixin.0.8.7"
            }]
        });
        let items = installer_library_items(&profile, &version, Path::new("staging/libraries"));
        assert_eq!(items.len(), 3, "去重后应是 3 个：{:?}", items.iter().map(|i| &i.label).collect::<Vec<_>>());
        let jspecify = items.iter().find(|i| i.label.contains("jspecify")).unwrap();
        assert_eq!(
            jspecify.mirror_url.as_deref(),
            Some("https://bmclapi2.bangbang93.com/maven/org/jspecify/jspecify/1.0.1/jspecify-1.0.1.jar")
        );
        assert!(jspecify
            .official_url
            .as_deref()
            .unwrap()
            .contains("maven.neoforged.net"));
        assert!(jspecify.dest.ends_with("jspecify-1.0.1.jar"));
        let mixin = items.iter().find(|i| i.label.contains("sponge-mixin")).unwrap();
        assert!(mixin.mirror_url.as_deref().unwrap().contains("bmclapi2.bangbang93.com/maven/"));
        assert!(mixin.mirror_url.as_deref().unwrap().contains("sponge-mixin"));
    }

    #[test]
    fn forge_and_fabric_install_artifacts_have_bmclapi_mirrors() {
        let forge = loaders::forge_installer_mirror_url("1.21.1", "52.0.0");
        assert_eq!(
            forge,
            "https://bmclapi2.bangbang93.com/maven/net/minecraftforge/forge/1.21.1-52.0.0/forge-1.21.1-52.0.0-installer.jar"
        );
        let fabric = loaders::bmclapi_maven_url("net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar");
        assert!(fabric.starts_with("https://bmclapi2.bangbang93.com/maven/net/fabricmc/"));
    }

    /// 真机：用已经下好的 NeoForge 安装器，经 BMCLAPI 补库后再跑 `--installClient`。
    ///
    /// `KALEID_NEOFORGE_STAGING` 指向一次失败安装留下的 staging（里面要有 installer.jar
    /// 和 `versions/<游戏版本>/<游戏版本>.jar`）。
    #[test]
    #[ignore = "联网，并用本机已有的 NeoForge 安装器验证补库后能装完"]
    fn real_neoforge_installer_succeeds_after_bmclapi_prefetch() {
        let Ok(staging_src) = std::env::var("KALEID_NEOFORGE_STAGING") else {
            eprintln!("没设 KALEID_NEOFORGE_STAGING，跳过");
            return;
        };
        let src = PathBuf::from(staging_src);
        assert!(src.join("installer.jar").is_file(), "staging 里没有 installer.jar");

        let work = tempfile::tempdir().unwrap();
        let staging = work.path().join("staging");
        copy_dir(&src, &staging);
        let installer = staging.join("installer.jar");
        let profile: Value = serde_json::from_slice(
            &crate::jar::read_entry_bytes(&installer, "install_profile.json", 2 * 1024 * 1024)
                .expect("读 install_profile.json"),
        )
        .unwrap();
        let version: Value = serde_json::from_slice(
            &crate::jar::read_entry_bytes(&installer, "version.json", 2 * 1024 * 1024)
                .expect("读 version.json"),
        )
        .unwrap();
        let mut items = installer_library_items(&profile, &version, &staging.join("libraries"));
        let total = items.len();
        items.retain(|item| !file_already_good(&item.dest, item.sha1.as_deref(), item.size));
        eprintln!("安装器依赖 {total} 个，还缺 {} 个，走 BMCLAPI", items.len());

        let settings = Arc::new(crate::settings::Settings::open(work.path()));
        let downloads = DownloadManager::new(settings);
        let job = Job::new(
            "neoforge-prefetch".into(),
            "补全 NeoForge 安装器依赖".into(),
            "neoforge".into(),
            "root".into(),
            "26.3".into(),
            kaleid_api::LoaderKind::NeoForge,
            None,
            kaleid_api::DOWNLOAD_SOURCE_MIRROR_FIRST.into(),
        );
        downloads.run_batch(job, items).expect("BMCLAPI 补库失败");

        let java = std::env::var("KALEID_JAVA").unwrap_or_else(|_| "java".into());
        let log = staging.join("installer-retry.log");
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let status = std::process::Command::new(java)
            .arg("-jar")
            .arg(&installer)
            .arg("--installClient")
            .arg(&staging)
            .current_dir(&staging)
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            .status()
            .expect("启动 Java 失败");
        let tail = tail_lines(&log, 30);
        assert!(
            status.success(),
            "安装器仍失败（{}）\n{tail}",
            status
        );
        eprintln!("{tail}");
    }

    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap().flatten() {
            let to = dst.join(entry.file_name());
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                copy_dir(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }
}
