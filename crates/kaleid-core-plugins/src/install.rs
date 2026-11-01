//! 安装编排：原版 / Fabric / Forge / NeoForge / Fabric API，以及自定义版本名。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

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
            "optifine" => loaders::optifine_versions(&self.remote, game),
            _ => Err(ApiError::invalid(
                "加载器只能是 fabric / forge / neoforge / optifine",
            )),
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
                        // OptiFine 换版本要同时改画像或 mods 里的 jar，走"重新安装"，
                        // 不走这里的原地更新。
                        LoaderKind::OptiFine => {
                            return Err(ApiError::invalid(
                                "OptiFine 不支持原地更新，请重新安装一个版本",
                            ))
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
        let requested_family = loader_family(requested);
        let list = self.versions.scan(Path::new(&root.path))?;
        for v in list {
            if v.inherits_from.as_deref() != Some(game) {
                continue;
            }
            if v.loader == LoaderKind::Vanilla {
                continue;
            }
            if loader_family(v.loader) != requested_family {
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
        job.set_current("读取版本 JSON…");
        let policy = job.policy().to_string();
        // 失败时要把这次新建的版本目录收干净（见 `InstallGuard`）。
        let mut guard = InstallGuard::new();
        guard.track(&loader_version_dir(root, &req.version_name));
        // 官方安装器的临时工作区也要收：里面可能躺着几百 MB 的 jar。
        if let Some(slug) = modded_staging_slug(&req.loader) {
            guard.track(&self.data_root.join("staging").join(format!("{slug}-{}", job.id)));
        }
        let value = self.game_version_json(&req.game_version, &policy)?;
        let mut core = self.vanilla_core_items(root, &req.version_name, &req.game_version, &value)?;

        match &req.loader {
            InstallLoader::Vanilla => {}
            InstallLoader::Fabric { version } => {
                // 自包含布局：原版与加载器全部落在 `versions/<版本名>/` 里，
                // 不再额外造一个 `versions/<原版 GameVersion>/` 当 inheritsFrom 的父级。
                core.extend(self.fabric_items(
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
                            core.push(DownloadItem {
                                label: format!("Fabric API {}", api.version),
                                phase: "fabric-api".into(),
                                official_url: Some(api.download_url.clone()),
                                // Fabric API 只落在 Modrinth 的 CDN 上，而那个域名在部分网络里
                                // 连不上（典型是 os error 10060）。MCIM 镜像能按前缀拿到同一
                                // 份文件，所以这里补一条镜像地址；到底先用哪个仍由下载源策略
                                // 决定（`ordered_urls`），官方优先时官方失败会立刻换过来。
                                mirror_url: kaleid_api::mirror_url_for(&api.download_url),
                                fallback_url: None,
                                prefer_mirror: false,
                                dest: mods.join(&api.file_name),
                                size: api.size,
                                sha1: api.sha1.clone(),
                            });
                        }
                    }
                }
            }
            InstallLoader::Forge { version } => {
                core.extend(self.forge_items(job, &req.game_version, version)?);
            }
            InstallLoader::Neoforge { version } => {
                core.extend(self.neoforge_items(job, &req.game_version, version)?);
            }
        }

        if let Some(optifine) = &req.optifine {
            validate_optifine(req, optifine)?;
            let with_forge = matches!(req.loader, InstallLoader::Forge { .. });
            core.extend(self.optifine_items(
                root,
                &req.version_name,
                &req.game_version,
                optifine,
                with_forge,
            )?);
        }

        job.complete_phase("prepare");
        for item in &core {
            job.add_total(&item.phase, item.size, 1);
        }
        job.set_status(kaleid_api::DownloadStatus::Queued, "准备下载");

        // 客户端与库先开下；资源索引在另一个线程里读，读完再把资源对象追加进来。
        let session = self.downloads.begin_session(job.clone());
        for item in core {
            session.push(item)?;
        }
        match self.vanilla_asset_items(root, &req.game_version, &value, &policy) {
            Ok(assets) => {
                for item in &assets {
                    job.add_total(&item.phase, item.size, 1);
                }
                for item in assets {
                    session.push(item)?;
                }
            }
            Err(error) => {
                // 资源索引拿不到时先让已排队的客户端/库下完，再把错误抛出去。
                let _ = session.close_and_wait();
                return Err(error);
            }
        }
        session.close_and_wait()?;

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

        // OptiFine 的收尾：单独装要改画像（launchwrapper + tweaker），配合 Forge
        // 只记一笔版本号（jar 已经在 mods/ 里，画像仍是 Forge）。
        if let Some(optifine) = &req.optifine {
            job.start_phase("optifine");
            job.set_current(format!("配置 OptiFine {}…", optifine.version()));
            if matches!(req.loader, InstallLoader::Vanilla) {
                apply_optifine_standalone(root, &req.version_name, &req.game_version, optifine)?;
            } else {
                record_optifine_mod(root, &req.version_name, optifine)?;
            }
            job.complete_phase("optifine");
        }
        // 走到这里才算装完：上面的任何一步失败都会让守卫把半成品删掉。
        guard.commit();
        job.complete();
        Ok(())
    }

    /// 按下载源策略取版本 JSON。镜像优先时不去等官方源超时。
    fn game_version_json(&self, game: &str, policy: &str) -> ApiResult<Value> {
        let manifest = self.remote.versions_with_policy(false, policy)?;
        let rv = manifest
            .versions
            .iter()
            .find(|v| v.id == game)
            .ok_or_else(|| ApiError::not_found(format!("版本清单中没有 {game}")))?;
        let mirror_json = format!(
            "https://bmclapi2.bangbang93.com/version/{}/json",
            loaders::url_encode(game)
        );
        let urls = crate::remote::meta_url_pair(&rv.url, &mirror_json, policy);
        // 走"必须真的是 JSON"的通道：镜像被限流时回的是 HTML 错误页，直接解析只会
        // 得到一句看不懂的 expected value。
        self.remote
            .fetch_verified_json(&urls, &format!("{game} 的版本 JSON"))
    }

    fn vanilla_core_items(
        &self,
        root: &Root,
        name: &str,
        game: &str,
        value: &Value,
    ) -> ApiResult<Vec<DownloadItem>> {
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
            prefer_mirror: false,
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
                    prefer_mirror: false,
                    dest: root_path.join("libraries").join(path),
                    size: artifact.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                    sha1: artifact
                        .get("sha1")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                });

                // 1.12.2 及更早的原生库在 `downloads.classifiers` 里，不进上面的
                // artifact 分支。漏掉它们 = `natives/` 空 = `no lwjgl` 启动失败。
                for native in crate::natives::native_jars(lib, crate::natives::current_os()) {
                    items.push(DownloadItem {
                        label: format!("原生库 {}", native.path),
                        phase: "libraries".into(),
                        official_url: Some(native.url.clone()),
                        mirror_url: Some(loaders::bmclapi_maven_url(&native.path)),
                        fallback_url: None,
                        prefer_mirror: false,
                        dest: root_path.join("libraries").join(&native.path),
                        size: native.size,
                        sha1: native.sha1.clone(),
                    });
                }
            }
        }
        Ok(items)
    }

    /// 资源索引与资源对象。它要等版本 JSON 读完才开始，所以单独一步，追加进已在跑的会话。
    fn vanilla_asset_items(
        &self,
        root: &Root,
        game: &str,
        value: &Value,
        policy: &str,
    ) -> ApiResult<Vec<DownloadItem>> {
        let root_path = Path::new(&root.path);
        let mut items = Vec::new();
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
                prefer_mirror: false,
                dest: root_path
                    .join("assets/indexes")
                    .join(format!("{index_id}.json")),
                size: index.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                sha1: index
                    .get("sha1")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            });
            let urls = crate::remote::meta_url_pair(index_url, &mirror_index, policy);
            // 资源索引是整次安装的命门：解析不出来就一个资源都排不进去。镜像被限流
            // 时会回 200 + HTML 错误页，所以必须走"验证过才解析"的通道。
            let index_value = self
                .remote
                .fetch_verified_json(&urls, &format!("资源索引 {index_id}"))?;
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
                        prefer_mirror: false,
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
                    prefer_mirror: false,
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
                    prefer_mirror: false,
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
        // 官方 Maven 在国内常年超时，`.sha1` 也常常一并超时——先问镜像那份。
        let sha1 = crate::remote::meta_url_pair(
            &format!("{}.sha1", loaders::forge_installer_mirror_url(game, version)),
            &loaders::forge_installer_sha1_url(game, version),
            job.policy(),
        );
        let sha1 = self
            .remote
            .fetch_text_ordered(&sha1)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(vec![DownloadItem {
            label: format!("Forge {version} 安装器"),
            phase: "installer".into(),
            official_url: Some(url),
            mirror_url: Some(loaders::forge_installer_mirror_url(game, version)),
            fallback_url: None,
            // 安装器只有这一个文件是"官方几乎连不上"的，其它库仍按玩家策略走。
            prefer_mirror: true,
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
        // 同 Forge：`.sha1` 先问镜像，官方那份常年超时。
        let sha1 = crate::remote::meta_url_pair(
            &format!("{}.sha1", loaders::neoforge_installer_mirror_url(version)),
            &loaders::neoforge_installer_sha1_url(version),
            job.policy(),
        );
        let sha1 = self
            .remote
            .fetch_text_ordered(&sha1)
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
            prefer_mirror: true,
            dest: installer,
            size: 0,
            sha1,
        }])
    }

    /// OptiFine 的下载条目。
    ///
    /// 两种形态完全不一样：
    /// - **配合 Forge**：OptiFine 就是个普通 mod，扔进 `mods/` 即可（1.8 起都这样，
    ///   不管游戏版本）。NeoForge / Fabric 都不行。
    /// - **单独装（只有 1.12.2 及更早）**：OptiFine 是"打了补丁的 jar"，靠
    ///   launchwrapper 的 `optifine.OptiFineTweaker` 注入，还要
    ///   `net.minecraft:launchwrapper:1.12` 与 `org.ow2.asm:asm-all:5.2` 两条库，
    ///   画像（mainClass / tweakClass）也得单独写。
    ///
    /// 无论哪种，文件都从 BMCLAPI 的 `/optifine/<游戏版本>/<类型>/<补丁号>` 取
    /// （官网要过 adfocus 跳转，抓不动），官方 `optifine.net` 只做兜底。
    fn optifine_items(
        &self,
        root: &Root,
        name: &str,
        game: &str,
        optifine: &kaleid_api::OptifineRef,
        with_forge: bool,
    ) -> ApiResult<Vec<DownloadItem>> {
        let version = optifine.version();
        let jar_name = loaders::optifine_jar_name(game, &version);
        let mirror = loaders::optifine_download_url(game, &optifine.kind, &optifine.patch);
        let official = loaders::optifine_official_url(&jar_name);
        let mut items = Vec::new();

        if with_forge {
            let global = self.settings.global()?;
            let isolated = kaleid_api::resolve_isolation(root.isolation, global.isolation_default);
            let mods = root.game_dir(name, isolated).join("mods");
            items.push(DownloadItem {
                label: format!("OptiFine {version}"),
                phase: "optifine".into(),
                official_url: Some(official),
                mirror_url: Some(mirror),
                fallback_url: None,
                // 官方那条要过跳转、还常常 404，直接先试 BMCLAPI。
                prefer_mirror: true,
                dest: mods.join(&jar_name),
                size: 0,
                sha1: None,
            });
            return Ok(items);
        }

        // 低版本：OptiFine 自己当一条库放进版本目录的 libraries/。
        let libraries = loader_version_dir(root, name).join("libraries");
        let relative = format!(
            "optifine/OptiFine/{}_{}/OptiFine-{}_{}.jar",
            game, version, game, version
        );
        items.push(DownloadItem {
            label: format!("OptiFine {version}"),
            phase: "optifine".into(),
            official_url: Some(official),
            mirror_url: Some(mirror),
            fallback_url: None,
            prefer_mirror: true,
            dest: libraries.join(&relative),
            size: 0,
            sha1: None,
        });
        // launchwrapper + asm-all：OptiFineTweaker 的运行时依赖，和 OptiFine 自己
        // 生成的 profile 里那两条一字不差。
        for dependency in ["net.minecraft:launchwrapper:1.12", "org.ow2.asm:asm-all:5.2"] {
            let Some(path) = library_path_from_name(dependency) else {
                continue;
            };
            items.push(DownloadItem {
                label: dependency.to_string(),
                phase: "optifine".into(),
                official_url: Some(format!("https://libraries.minecraft.net/{path}")),
                // 这两条在 MC 官方库上，BMCLAPI 的 /maven/ 同样镜像。
                mirror_url: Some(loaders::bmclapi_maven_url(&path)),
                fallback_url: None,
                prefer_mirror: false,
                dest: libraries.join(&path),
                size: 0,
                sha1: None,
            });
        }
        Ok(items)
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
        // 下到的可能根本不是 jar：镜像/服务端偶尔会回一个 HTML 错误页，
        // 或者干脆只写了个空文件。让它进 Java 只会换来一句
        // `Invalid or corrupt jarfile installer.jar`，玩家完全不知道下一步做什么。
        ensure_installer_jar(&staging.join("installer.jar"))?;
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
                        "{label} 安装器失败（{status}）。依赖已尽量从 BMCLAPI 补过，下面是安装器自己的输出：\n{tail}{}",
                        installer_failure_hint(&tail)
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
        prefer_mirror: false,
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

/// OptiFine 从 1.13 起改成"普通 mod"投放（必须配合 Forge）；1.12.2 及更早是
/// 打了补丁的 jar + launchwrapper tweaker。这个分界线决定了文件放哪、画像怎么写。
pub fn is_modern_optifine(game: &str) -> bool {
    let mut parts = game.split('.');
    let major = parts.next().and_then(|s| s.parse::<u32>().ok());
    let minor = parts.next().and_then(|s| s.parse::<u32>().ok());
    match (major, minor) {
        // 1.12.2 及以前的 1.x 一律算老版本；1.13+ 是新形态。
        (Some(1), Some(minor)) => minor >= 13,
        // 26.x 这类新编号（理论上没有 OptiFine）按新形态处理。
        (Some(major), Some(_)) if major >= 2 => true,
        _ => false,
    }
}

/// 「同一个游戏版本只能有一个加载器画像」按**家族**判定：
/// Forge 与 OptiFine 算一家——高版本里 OptiFine 本来就得挂在 Forge 上，
/// 低版本也常见「先装 Forge 再放 OptiFine」。
fn loader_family(kind: LoaderKind) -> LoaderKind {
    match kind {
        LoaderKind::OptiFine => LoaderKind::Forge,
        other => other,
    }
}

/// OptiFine 只能这么装：
/// - 和 **Forge** 一起（1.8 起的常规玩法，jar 进 `mods/`）；
/// - 或者 1.12.2 及更早**单独**装（launchwrapper + tweaker）。
///
/// Fabric / NeoForge 都不兼容 OptiFine；1.13+ 也没有"单独装"这条路（那会儿
/// OptiFine 已经完全是 mod 了）。
fn validate_optifine(req: &InstallRequest, optifine: &kaleid_api::OptifineRef) -> ApiResult<()> {
    if optifine.kind.trim().is_empty() || optifine.patch.trim().is_empty() {
        return Err(ApiError::invalid("OptiFine 版本不完整"));
    }
    match &req.loader {
        InstallLoader::Forge { .. } => Ok(()),
        InstallLoader::Fabric { .. } | InstallLoader::Neoforge { .. } => Err(ApiError::invalid(
            "OptiFine 只能和 Forge 一起安装（Fabric / NeoForge 不支持 OptiFine）",
        )),
        InstallLoader::Vanilla => {
            if is_modern_optifine(&req.game_version) {
                Err(ApiError::invalid(format!(
                    "{} 的 OptiFine 是 mod，必须和 Forge 一起安装；请先选择 Forge 再加 OptiFine。",
                    req.game_version
                )))
            } else {
                Ok(())
            }
        }
    }
}

/// 低版本单独装 OptiFine 时要把画像改成 launchwrapper + tweaker。
///
/// OptiFine 自己的安装器产出的 profile 就是这个形状：`mainClass` 换成
/// `net.minecraft.launchwrapper.Launch`，末尾加一条
/// `--tweakClass optifine.OptiFineTweaker`，再把 OptiFine / launchwrapper /
/// asm-all 三条库挂进去。
fn apply_optifine_standalone(
    root: &Root,
    name: &str,
    game: &str,
    optifine: &kaleid_api::OptifineRef,
) -> ApiResult<()> {
    let dir = loader_version_dir(root, name);
    let mut value = read_version_json(&dir, name)?;
    let version = optifine.version();
    value["mainClass"] = json!("net.minecraft.launchwrapper.Launch");

    // legacy 版本用 `minecraftArguments` 字符串；少数版本用 `arguments.game` 数组。
    let tweak = "optifine.OptiFineTweaker";
    if let Some(args) = value.get("minecraftArguments").and_then(|v| v.as_str()) {
        if !args.contains("--tweakClass") {
            value["minecraftArguments"] = json!(format!("{args} --tweakClass {tweak}"));
        }
    } else {
        let game_args = value
            .get_mut("arguments")
            .and_then(|a| a.get_mut("game"))
            .and_then(|g| g.as_array_mut());
        match game_args {
            Some(list) => {
                if !list.iter().any(|v| v == "--tweakClass") {
                    list.push(json!("--tweakClass"));
                    list.push(json!(tweak));
                }
            }
            None => {
                value["minecraftArguments"] = json!(format!(
                    "--username ${{auth_player_name}} --version ${{version_name}} \
                     --gameDir ${{game_directory}} --assetsDir ${{assets_root}} \
                     --assetIndex ${{assets_index_name}} --uuid ${{auth_uuid}} \
                     --userProperties ${{user_properties}} --userType ${{user_type}} \
                     --tweakClass {tweak}"
                ));
            }
        }
    }

    // 三条库：OptiFine 本体（我们自己的路径）+ launchwrapper + asm-all。
    let libraries = value
        .get_mut("libraries")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| ApiError::Format(format!("{name} 的版本 JSON 没有 libraries")))?;
    let mut push_lib = |entry: Value| {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if !libraries
            .iter()
            .any(|l| l.get("name").and_then(|v| v.as_str()) == Some(name))
        {
            libraries.push(entry);
        }
    };
    push_lib(json!({
        "name": loaders::optifine_library_coordinate(game, &version),
        "downloads": { "artifact": {
            "path": format!("optifine/OptiFine/{game}_{version}/OptiFine-{game}_{version}.jar"),
            "size": 0,
            "url": loaders::optifine_download_url(game, &optifine.kind, &optifine.patch),
        }},
    }));
    push_lib(json!({"name": "net.minecraft:launchwrapper:1.12"}));
    push_lib(json!({"name": "org.ow2.asm:asm-all:5.2"}));

    // 记录 OptiFine 版本，侧栏/更新检查都要读它。
    value["kaleidOptifine"] = json!({
        "version": version,
        "kind": optifine.kind,
        "patch": optifine.patch,
        "mode": "standalone",
    });
    write_version_json(root, name, &value)?;
    write_loader_marker(&dir, "optifine", &version, game)?;
    Ok(())
}

/// 配合 Forge 装 OptiFine 时只记一笔版本号：jar 在 `mods/` 里，画像仍是 Forge。
fn record_optifine_mod(
    root: &Root,
    name: &str,
    optifine: &kaleid_api::OptifineRef,
) -> ApiResult<()> {
    let dir = loader_version_dir(root, name);
    let mut value = read_version_json(&dir, name)?;
    value["kaleidOptifine"] = json!({
        "version": optifine.version(),
        "kind": optifine.kind,
        "patch": optifine.patch,
        "mode": "mod",
    });
    write_version_json(root, name, &value)
}

fn tail_lines(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 安装器落盘后确认它真的是个 jar（ZIP 开头 `PK\x03\x04`）。
///
/// 镜像或 CDN 出错时经常回一页 HTML、或者只留个 0 字节文件；直接交给 Java 会
/// 得到 `Invalid or corrupt jarfile`，这句话对玩家没有任何指引作用。
fn ensure_installer_jar(path: &Path) -> ApiResult<()> {
    let bytes = std::fs::read(path)
        .map_err(|e| ApiError::Network(format!("安装器没有下载成功（{}）：{e}", path.display())))?;
    if bytes.starts_with(b"PK\x03\x04") {
        return Ok(());
    }
    let looks_like_html = {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
        let head = head.trim_start().to_ascii_lowercase();
        head.starts_with("<!doctype") || head.starts_with("<html")
    };
    let detail = if bytes.is_empty() {
        "下载到的是空文件".to_string()
    } else if looks_like_html {
        "下载到的是网页（HTML），不是 jar".to_string()
    } else {
        format!("文件只有 {} 字节且不是 jar", bytes.len())
    };
    Err(ApiError::Network(format!(
        "安装器不是有效的 jar：{detail}。多半是下载源返回了错误页面，\
         请到「设置 → 下载」换一个下载源（推荐「BMCLAPI 优先」）后重试。"
    )))
}

/// 安装器失败时，把日志尾部翻译成一句"下一步怎么办"。
///
/// 安装器自己会去 `maven.neoforged.net` / `maven.minecraftforge.net` 拉缺失的库，
/// 这两台在国内经常直接 Connect timed out——日志里出现域名或超时，就是它，
/// 而不是玩家的 Java 有问题。
fn installer_failure_hint(tail: &str) -> String {
    let lower = tail.to_ascii_lowercase();
    let official_maven_dead = lower.contains("maven.neoforged.net")
        || lower.contains("maven.minecraftforge.net")
        || lower.contains("connect timed out")
        || lower.contains("connection timed out")
        || lower.contains("unknownhost")
        || lower.contains("certificate");
    if official_maven_dead {
        "\n\n提示：日志里出现了官方 Maven 的域名或连接超时，说明它连不上（国内很常见）。\
         请把下载源改成「BMCLAPI 优先」后重试。"
            .to_string()
    } else {
        String::new()
    }
}

fn write_version_json(root: &Root, name: &str, value: &Value) -> ApiResult<()> {
    let dir = loader_version_dir(root, name);
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(dir.join(format!("{name}.json")), text)?;
    Ok(())
}

/// 一次安装的"施工现场"清理器。
///
/// 安装失败时最烦的不是那句报错，而是 `versions/<名字>/` 里剩下半个版本：版本列表
/// 里看得见、点启动就炸，用户还得自己翻目录去删。这个守卫只登记**本次新建**的
/// 目录（已经存在的一律不碰，绝不删用户的东西），失败或取消时把它们整个删掉，
/// 成功之后才放手。
struct InstallGuard {
    dirs: Vec<PathBuf>,
    keep: bool,
}

impl InstallGuard {
    fn new() -> Self {
        InstallGuard {
            dirs: Vec::new(),
            keep: false,
        }
    }

    /// 登记一个目录。必须在创建它**之前**调用，否则会误判成"原本就有"。
    fn track(&mut self, dir: &Path) {
        if !dir.exists() && !self.dirs.iter().any(|tracked| tracked == dir) {
            self.dirs.push(dir.to_path_buf());
        }
    }

    /// 安装成功：从外到内保留所有痕迹。
    fn commit(mut self) {
        self.keep = true;
    }
}

impl Drop for InstallGuard {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        for dir in &self.dirs {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// 清扫上次崩溃/被杀留下的 staging 目录。
///
/// `<data_root>/staging` 是安装器和资源索引的临时工作区，正常结束会自己删掉；但
/// 进程被强杀或断电时留下的目录会一直堆着（里面可能有几百 MB 的 jar）。启动时顺手
/// 清掉超过 24 小时的。
pub fn sweep_staging(data_root: &Path) {
    sweep_staging_at(
        data_root,
        SystemTime::now(),
        Duration::from_secs(STAGING_MAX_AGE_SECS),
    );
}

fn sweep_staging_at(data_root: &Path, now: SystemTime, max_age: Duration) {
    let base = data_root.join("staging");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|at| {
                now.duration_since(at)
                    .map(|age| age > max_age)
                    .unwrap_or(false)
            })
            // 读不到时间就不动它：宁可留一堆目录，也不能删掉正在跑的安装。
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// staging 目录最多留一天。
const STAGING_MAX_AGE_SECS: u64 = 24 * 3600;

/// Forge / NeoForge 的 staging 目录名（`staging/<slug>-<job>`）；原版和 Fabric 没有。
fn modded_staging_slug(loader: &InstallLoader) -> Option<&'static str> {
    match loader {
        InstallLoader::Forge { .. } => Some("forge"),
        InstallLoader::Neoforge { .. } => Some("neoforge"),
        _ => None,
    }
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

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kaleid-install-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 安装失败（下载到一半、安装器退出码非 0、用户取消……）之后，不能留下一个
    /// "版本列表里看得见、点启动就炸"的半成品目录。
    #[test]
    fn failed_install_leaves_no_version_dir() {
        let root = scratch("failed");
        let version_dir = root.join("versions").join("1.21.4-fabric");
        {
            let mut guard = InstallGuard::new();
            guard.track(&version_dir);
            // 模拟「版本 JSON 已写盘、client.jar 还没下完就失败了」。
            std::fs::create_dir_all(&version_dir).unwrap();
            std::fs::write(version_dir.join("1.21.4-fabric.json"), "{}").unwrap();
            std::fs::create_dir_all(version_dir.join("libraries")).unwrap();
        }
        assert!(
            !version_dir.exists(),
            "失败后整个版本目录都该消失：{}",
            version_dir.display()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 已经存在的版本目录一个字节都不能动——用户自己的存档式安装不能被守卫误删。
    #[test]
    fn guard_never_touches_a_directory_that_already_existed() {
        let root = scratch("existing");
        let version_dir = root.join("versions").join("1.21.4");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("keep.txt"), "用户的文件").unwrap();
        {
            let mut guard = InstallGuard::new();
            guard.track(&version_dir);
            std::fs::write(version_dir.join("ours.json"), "{}").unwrap();
        }
        assert!(version_dir.join("keep.txt").is_file(), "原有文件必须还在");
        assert!(version_dir.join("ours.json").is_file(), "原有目录不会被整体删除");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn successful_install_keeps_everything() {
        let root = scratch("ok");
        let version_dir = root.join("versions").join("1.21.4");
        {
            let mut guard = InstallGuard::new();
            guard.track(&version_dir);
            std::fs::create_dir_all(&version_dir).unwrap();
            std::fs::write(version_dir.join("1.21.4.json"), "{}").unwrap();
            guard.commit();
        }
        assert!(version_dir.join("1.21.4.json").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 进程被强杀留下的 staging 要在下次启动时清掉，但正在跑的那次不能碰。
    #[test]
    fn sweep_staging_removes_only_old_dirs() {
        let data_root = scratch("sweep");
        let staging = data_root.join("staging");
        let old_dir = staging.join("forge-oldjob");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("installer.jar"), b"jar").unwrap();
        // 目录的修改时间就是"这次安装最后一次动它"的时刻，睡觉来制造新旧差异。
        std::thread::sleep(Duration::from_millis(700));
        let fresh_dir = staging.join("neoforge-running");
        std::fs::create_dir_all(&fresh_dir).unwrap();
        sweep_staging_at(
            &data_root,
            SystemTime::now(),
            Duration::from_millis(500),
        );
        assert!(!old_dir.exists(), "超过阈值的空壳要删掉");
        assert!(fresh_dir.exists(), "刚建的不能碰");
        let _ = std::fs::remove_dir_all(&data_root);
    }

    /// 真机：BMCLAPI 的 OptiFine 列表与直链（老版本单装 / 高版本 mod 两条路都要通）。
    ///
    /// `cargo test -p kaleid-core-plugins real_optifine -- --ignored --nocapture`
    #[test]
    #[ignore = "联网，验证 BMCLAPI 的 OptiFine 列表与 jar 直链"]
    fn real_optifine_list_and_jar_are_reachable() {
        use std::io::Read;

        let client = crate::remote::RemoteClient::new();
        for game in ["1.12.2", "1.21.4"] {
            let list = loaders::optifine_versions(&client, game)
                .unwrap_or_else(|e| panic!("{game} 取不到 OptiFine 列表：{e}"));
            assert!(!list.versions.is_empty(), "{game} 的 OptiFine 列表是空的");
            let chosen = list
                .versions
                .iter()
                .find(|v| v.stable)
                .or_else(|| list.versions.first())
                .unwrap();
            let parsed = kaleid_api::OptifineRef::from_version(&chosen.version)
                .unwrap_or_else(|| panic!("版本号解析不了：{}", chosen.version));
            let url = loaders::optifine_download_url(game, &parsed.kind, &parsed.patch);
            let response = client
                .agent()
                .get(&url)
                .call()
                .unwrap_or_else(|e| panic!("{url} 打不开：{e}"));
            let mut bytes = Vec::new();
            response.into_reader().read_to_end(&mut bytes).unwrap();
            assert!(
                bytes.starts_with(b"PK\x03\x04"),
                "{url} 返回的不是 jar（{} 字节）",
                bytes.len()
            );
            eprintln!("{} {} → {} 字节", game, chosen.version, bytes.len());
        }
    }

    /// 一个最小可用的服务 + 根目录，给需要 `optifine_items` / 画像写盘的单测用。
    fn service_with_root() -> (tempfile::TempDir, Root, InstallService) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Root {
            id: "r".into(),
            path: tmp.path().to_string_lossy().to_string(),
            kind: kaleid_api::RootKind::BareMinecraft,
            alias: "本地".into(),
            added_at: "0".into(),
            isolation: kaleid_api::IsolationMode::Off,
            auto: false,
            last_used: None,
        };
        let settings = Arc::new(Settings::open(tmp.path()));
        let versions = Arc::new(VersionStore::new());
        let roots = Arc::new(RootRegistry::open(tmp.path(), None).unwrap());
        let downloads = Arc::new(DownloadManager::new(settings.clone()));
        let service = InstallService::new(settings, versions, roots, downloads, tmp.path());
        (tmp, root, service)
    }

    fn optifine_request(loader: InstallLoader, game: &str) -> InstallRequest {
        InstallRequest {
            root_id: "r".into(),
            game_version: game.into(),
            version_name: format!("{game}-optifine"),
            loader,
            install_fabric_api: false,
            optifine: Some(kaleid_api::OptifineRef {
                kind: "HD_U".into(),
                patch: "J3".into(),
            }),
        }
    }

    /// OptiFine 的分界线：1.13 起是 mod，1.12.2 及更早靠 launchwrapper。
    #[test]
    fn optifine_modern_boundary() {
        for old in ["1.7.10", "1.8.9", "1.12.2", "1.12", "1.6.4"] {
            assert!(!is_modern_optifine(old), "{old} 应该是老形态");
        }
        for modern in ["1.13", "1.16.5", "1.20.1", "1.21.4", "26.1.2"] {
            assert!(is_modern_optifine(modern), "{modern} 应该是 mod 形态");
        }
        assert!(!is_modern_optifine("nonsense"));
    }

    /// 高版本 OptiFine 只能和 Forge 一起装；低版本可以单独装。
    #[test]
    fn optifine_requires_forge_on_modern_versions() {
        let of = kaleid_api::OptifineRef {
            kind: "HD_U".into(),
            patch: "J3".into(),
        };
        // Fabric / NeoForge 一律不行。
        for loader in [
            InstallLoader::Fabric {
                version: "0.16.9".into(),
            },
            InstallLoader::Neoforge {
                version: "21.1.72".into(),
            },
        ] {
            let req = optifine_request(loader, "1.21.4");
            assert!(validate_optifine(&req, &of).is_err());
        }
        // 1.21.4 单独装 OptiFine：不行，提示要先选 Forge。
        let req = optifine_request(InstallLoader::Vanilla, "1.21.4");
        let err = validate_optifine(&req, &of).unwrap_err().to_string();
        assert!(err.contains("Forge"), "{err}");
        // 1.21.4 + Forge：可以。
        let req = optifine_request(
            InstallLoader::Forge {
                version: "54.1.22".into(),
            },
            "1.21.4",
        );
        assert!(validate_optifine(&req, &of).is_ok());
        // 1.12.2 单独装：可以。
        let req = optifine_request(InstallLoader::Vanilla, "1.12.2");
        assert!(validate_optifine(&req, &of).is_ok());
    }

    /// 配合 Forge 时 OptiFine 只进 `mods/`，不碰画像。
    #[test]
    fn optifine_goes_to_mods_when_forge_is_present() {
        let (_tmp, root, service) = service_with_root();
        let of = kaleid_api::OptifineRef {
            kind: "HD_U".into(),
            patch: "J3".into(),
        };
        let items = service
            .optifine_items(&root, "1.21.4-forge-optifine", "1.21.4", &of, true)
            .unwrap();
        assert_eq!(items.len(), 1, "Forge 路线只该有一条 OptiFine");
        let item = &items[0];
        assert!(item
            .dest
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("mods/OptiFine_1.21.4_HD_U_J3.jar"));
        assert_eq!(
            item.mirror_url.as_deref(),
            Some("https://bmclapi2.bangbang93.com/optifine/1.21.4/HD_U/J3")
        );
        assert!(item.prefer_mirror, "官网那条要过跳转，应该先试 BMCLAPI");
    }

    /// 单独装（1.12.2）时要带上 launchwrapper 与 asm-all，并且画像改成 tweaker。
    #[test]
    fn optifine_standalone_writes_a_launchwrapper_profile() {
        let (_tmp, root, service) = service_with_root();
        let of = kaleid_api::OptifineRef {
            kind: "HD_U".into(),
            patch: "G5".into(),
        };
        // 先铺一份 1.12.2 形状的版本 JSON（vanilla_core_items 就是这么写的）。
        let dir = loader_version_dir(&root, "1.12.2-optifine");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("1.12.2-optifine.json"),
            serde_json::to_string_pretty(&json!({
                "id": "1.12.2-optifine",
                "mainClass": "net.minecraft.client.main.Main",
                "minecraftArguments": "--username ${auth_player_name} --gameDir ${game_directory}",
                "libraries": [{"name": "com.mojang:logging:1.12.2"}]
            }))
            .unwrap(),
        )
        .unwrap();

        let items = service
            .optifine_items(&root, "1.12.2-optifine", "1.12.2", &of, false)
            .unwrap();
        // OptiFine 本体 + launchwrapper + asm-all
        assert_eq!(items.len(), 3, "{:?}", items.iter().map(|i| &i.label).collect::<Vec<_>>());
        assert!(items
            .iter()
            .any(|i| i.dest.to_string_lossy().replace('\\', "/")
                .ends_with("libraries/optifine/OptiFine/1.12.2_HD_U_G5/OptiFine-1.12.2_HD_U_G5.jar")));
        assert!(items
            .iter()
            .any(|i| i.dest.to_string_lossy().replace('\\', "/")
                .ends_with("launchwrapper-1.12.jar")));
        assert!(items
            .iter()
            .any(|i| i.dest.to_string_lossy().replace('\\', "/").ends_with("asm-all-5.2.jar")));

        apply_optifine_standalone(&root, "1.12.2-optifine", "1.12.2", &of).unwrap();
        let value: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("1.12.2-optifine.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["mainClass"], "net.minecraft.launchwrapper.Launch");
        let args = value["minecraftArguments"].as_str().unwrap();
        assert!(args.contains("--tweakClass optifine.OptiFineTweaker"), "{args}");
        let names: Vec<String> = value["libraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "optifine:OptiFine:1.12.2_HD_U_G5"), "{names:?}");
        assert!(names.iter().any(|n| n == "net.minecraft:launchwrapper:1.12"), "{names:?}");
        assert!(names.iter().any(|n| n == "org.ow2.asm:asm-all:5.2"), "{names:?}");
        assert_eq!(value["kaleidOptifine"]["mode"], "standalone");
        assert!(dir.join(".optifine/loader.json").is_file(), "要留标记目录");
    }

    /// 版本 JSON 一到手，客户端与依赖库就该先排进下载队列，不用等整份资源索引。
    ///
    /// 这是「解析版本很慢」的直接修复点：以前 `vanilla_items` 要把资源索引读完、
    /// 攒齐几千个对象才开跑，界面上一直停在「解析版本信息…」。
    #[test]
    fn vanilla_core_items_are_ready_before_the_asset_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Root {
            id: "r".into(),
            path: tmp.path().to_string_lossy().to_string(),
            kind: kaleid_api::RootKind::BareMinecraft,
            alias: "本地".into(),
            added_at: "0".into(),
            isolation: kaleid_api::IsolationMode::Off,
            auto: false,
            last_used: None,
        };
        let settings = Arc::new(Settings::open(tmp.path()));
        let versions = Arc::new(VersionStore::new());
        let roots = Arc::new(RootRegistry::open(tmp.path(), None).unwrap());
        let downloads = Arc::new(DownloadManager::new(settings.clone()));
        let service = InstallService::new(
            settings,
            versions,
            roots,
            downloads,
            tmp.path(),
        );
        let value = json!({
            "id": "26.1.2",
            "downloads": {
                "client": {
                    "url": "https://piston-data.mojang.com/v1/objects/abc/client.jar",
                    "size": 27_000_000u64,
                    "sha1": "a".repeat(40)
                }
            },
            "libraries": [
                {"name": "org.lwjgl:lwjgl:3.3.3",
                 "downloads": {"artifact": {
                     "path": "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar",
                     "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar",
                     "size": 100u64}}},
                {"name": "other:os-only:1", "rules": [{"action": "allow", "os": {"name": "linux"}}],
                 "downloads": {"artifact": {"path": "x/y.jar", "url": "https://example.invalid/y.jar"}}}
            ]
        });

        let items = service
            .vanilla_core_items(&root, "26.1.2-测试", "26.1.2", &value)
            .expect("核心下载项应当能单独建出来");

        // 客户端 + 当前系统上可用的那个库；别的系统的库不该出现。
        assert_eq!(items.len(), 2, "got {:?}", items.iter().map(|i| &i.label).collect::<Vec<_>>());
        assert_eq!(items[0].phase, "client");
        assert!(items[0].mirror_url.as_deref().unwrap().contains("bmclapi2"));
        assert_eq!(items[1].phase, "libraries");
        assert!(items[1].mirror_url.as_deref().unwrap().contains("bmclapi2"));
        // 版本 JSON 已经落盘，加载器那一步可以直接读取合并。
        assert!(crate::install::loader_version_dir(&root, "26.1.2-测试")
            .join("26.1.2-测试.json")
            .is_file());
    }

    /// 1.12.2 及更早的原生库在 `downloads.classifiers` 里，不在 `downloads.artifact`
    /// 分支上。不把它们排进下载队列，`natives/` 就是空的，启动必然
    /// `java.lang.UnsatisfiedLinkError: no lwjgl in java.library.path`。
    #[test]
    fn vanilla_core_items_include_classifier_natives() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Root {
            id: "r".into(),
            path: tmp.path().to_string_lossy().to_string(),
            kind: kaleid_api::RootKind::BareMinecraft,
            alias: "本地".into(),
            added_at: "0".into(),
            isolation: kaleid_api::IsolationMode::Off,
            auto: false,
            last_used: None,
        };
        let settings = Arc::new(Settings::open(tmp.path()));
        let service = InstallService::new(
            settings,
            Arc::new(VersionStore::new()),
            Arc::new(RootRegistry::open(tmp.path(), None).unwrap()),
            Arc::new(DownloadManager::new(Arc::new(Settings::open(tmp.path())))),
            tmp.path(),
        );
        let value = json!({
            "id": "1.5.2",
            "downloads": {"client": {
                "url": "https://piston-data.mojang.com/v1/objects/abc/client.jar",
                "size": 1u64,
                "sha1": "a".repeat(40)
            }},
            "libraries": [{
                "name": "org.lwjgl.lwjgl:lwjgl-platform:2.9.0",
                "natives": {"windows": "natives-windows", "linux": "natives-linux", "osx": "natives-osx"},
                "extract": {"exclude": ["META-INF/"]},
                "downloads": {
                    "artifact": {
                        "path": "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0.jar",
                        "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0.jar"
                    },
                    "classifiers": {
                        "natives-windows": {
                            "path": "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-windows.jar",
                            "url": "https://libraries.minecraft.net/org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-windows.jar",
                            "size": 578_680u64,
                            "sha1": "b".repeat(40)
                        },
                        "natives-linux": {
                            "path": "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-linux.jar"
                        },
                        "natives-osx": {
                            "path": "org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-osx.jar"
                        }
                    }
                }
            }]
        });

        let items = service
            .vanilla_core_items(&root, "1.5.2-原生库", "1.5.2", &value)
            .expect("核心下载项应当能建出来");

        // 客户端 + 基础 jar，还有且只有一个「当前平台」的原生包。
        let natives: Vec<&DownloadItem> = items
            .iter()
            .filter(|item| item.label.starts_with("原生库"))
            .collect();
        assert_eq!(
            natives.len(),
            1,
            "只该排当前平台的原生包，got {:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        let expected = format!(
            "libraries/org/lwjgl/lwjgl/lwjgl-platform/2.9.0/lwjgl-platform-2.9.0-natives-{}.jar",
            crate::natives::current_os()
        );
        assert!(
            natives[0]
                .dest
                .to_string_lossy()
                .replace('\\', "/")
                .ends_with(&expected),
            "落盘位置不对：{}",
            natives[0].dest.display()
        );
        assert_eq!(natives[0].phase, "libraries");
        assert!(natives[0]
            .mirror_url
            .as_deref()
            .unwrap_or_default()
            .contains("bmclapi2"));
    }

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

    /// 下到 HTML 错误页 / 空文件时，报错要说人话（不是让 Java 抛 Invalid jarfile）。
    #[test]
    fn installer_jar_must_be_a_zip() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.jar");
        std::fs::write(&good, b"PK\x03\x04rest-of-a-zip").unwrap();
        assert!(ensure_installer_jar(&good).is_ok());

        let html = dir.path().join("html.jar");
        std::fs::write(&html, b"<!DOCTYPE html><html><body>403 Forbidden</body></html>").unwrap();
        let err = ensure_installer_jar(&html).unwrap_err().to_string();
        assert!(err.contains("不是有效的 jar"), "{err}");
        assert!(err.contains("下载源"), "要指向下一步动作：{err}");

        let empty = dir.path().join("empty.jar");
        std::fs::write(&empty, b"").unwrap();
        assert!(ensure_installer_jar(&empty)
            .unwrap_err()
            .to_string()
            .contains("空文件"));

        let missing = dir.path().join("nope.jar");
        assert!(ensure_installer_jar(&missing).is_err());
    }

    /// 安装器日志里出现官方 Maven 域名/超时时，错误信息要告诉玩家换下载源。
    #[test]
    fn installer_failure_hint_points_at_the_download_source() {
        let tail = "[main/ERROR] Could not download libraries\njava.net.ConnectException: Connect timed out: maven.neoforged.net";
        let hint = installer_failure_hint(tail);
        assert!(hint.contains("BMCLAPI"), "{hint}");
        // 与网络无关的失败（例如 Java 版本不对）不要乱指路。
        assert!(installer_failure_hint("UnsupportedClassVersionError: 65").is_empty());
    }

    /// 真机：安装器在「BMCLAPI 优先」下能不能拿到一个**真的** jar。
    ///
    /// 这是"NeoForge 装不上"的直接证据收集：`maven.neoforged.net` 在国内经常
    /// 直接超时，这个测试按"镜像先、官方后"的顺序试，打印每条 URL 的结果。
    /// 命令：`cargo test -p kaleid-core-plugins real_neoforge_installer_downloads_via_bmclapi
    /// -- --ignored --nocapture`（可用 `KALEID_NEOFORGE_VERSION` 换版本）。
    #[test]
    #[ignore = "联网，验证安装器能从 BMCLAPI 下到一个真正的 jar"]
    fn real_neoforge_installer_downloads_via_bmclapi() {
        use std::io::Read;

        let version = std::env::var("KALEID_NEOFORGE_VERSION")
            .unwrap_or_else(|_| "26.3.0.99".to_string());
        let candidates = vec![
            loaders::neoforge_installer_mirror_url(&version),
            loaders::neoforge_installer_url(&version),
        ];
        let client = crate::remote::RemoteClient::new();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("installer.jar");
        let mut errors: Vec<String> = Vec::new();
        for url in &candidates {
            match client.agent().get(url).call() {
                Ok(response) => {
                    let mut bytes = Vec::new();
                    if let Err(e) = response.into_reader().read_to_end(&mut bytes) {
                        errors.push(format!("{url}: 读取失败 {e}"));
                        continue;
                    }
                    std::fs::write(&dest, &bytes).unwrap();
                    match ensure_installer_jar(&dest) {
                        Ok(()) => {
                            eprintln!("{} → {} 字节，是有效 jar", url, bytes.len());
                            return;
                        }
                        Err(e) => errors.push(format!("{url}: {e}（{} 字节）", bytes.len())),
                    }
                }
                Err(e) => errors.push(format!("{url}: {e}")),
            }
        }
        panic!("没有任何候选源拿到有效安装器：\n{}", errors.join("\n"));
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
