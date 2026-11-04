//! `ContentService::apply_update` 的端到端测试（本地假站，不联网）。
//!
//! 这段逻辑最容易出「半个 jar 留在 mods 里」或「旧文件被删掉回不去」的事故，
//! 所以宁可自己起一个只会吐一个文件的 HTTP 服务，也不把它们留给手工验证。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use kaleid_api::{IsolationMode, ModSource, ModUpdate, RootRegistryService};
use kaleid_core_plugins::{
    ContentService, DownloadManager, ModNames, RootRegistry, Settings, VersionStore,
};

/// 清单里每个文件都回同一份字节的迷你 HTTP 服务。
struct FileServer {
    base: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FileServer {
    fn start(body: Vec<u8>) -> FileServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let body = Arc::new(body);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let body = body.clone();
                            std::thread::spawn(move || serve(stream, &body));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        FileServer {
            base,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn serve(mut stream: TcpStream, body: &[u8]) {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                head.extend_from_slice(&buf[..n]);
                if head.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: none\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

struct Fixture {
    _dir: tempfile::TempDir,
    svc: ContentService,
    root_id: String,
    mods_dir: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let game = dir.path().join(".minecraft");
    std::fs::create_dir_all(game.join("versions/1.21.4-fabric")).unwrap();
    std::fs::write(
        game.join("versions/1.21.4-fabric/1.21.4-fabric.json"),
        serde_json::to_string(&serde_json::json!({
            "id": "1.21.4-fabric",
            "inheritsFrom": "1.21.4",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "arguments": { "game": [] }
        }))
        .unwrap(),
    )
    .unwrap();

    let roots = Arc::new(RootRegistry::empty_at(&data).unwrap());
    let root = roots.add(&game, Some("测试"), true).unwrap();
    roots.set_isolation(&root.id, IsolationMode::On).unwrap();
    let settings = Arc::new(Settings::open(&data));
    let downloads = Arc::new(DownloadManager::new(settings.clone()));
    let svc = ContentService::new(settings, roots.clone(), Arc::new(ModNames::default()))
        .with_downloads(Some(downloads))
        .with_versions(Some(Arc::new(VersionStore::new())));
    let mods_dir = roots
        .get(&root.id)
        .unwrap()
        .game_dir("1.21.4-fabric", true)
        .join("mods");
    std::fs::create_dir_all(&mods_dir).unwrap();
    Fixture {
        _dir: dir,
        svc,
        root_id: root.id,
        mods_dir,
    }
}

fn update_for(url: String, size: u64, latest_name: &str) -> ModUpdate {
    ModUpdate {
        file_name: "sodium-0.6.0.jar".into(),
        mod_id: Some("sodium".into()),
        source: ModSource::Modrinth.as_str().to_string(),
        project_id: "AANobbMI".into(),
        current_version: Some("0.6.0".into()),
        latest_version: "0.6.9".into(),
        latest_file_name: latest_name.into(),
        download_url: url,
        size,
        sha1: None,
        page_url: None,
    }
}

/// 备份目录里的文件名（`mods/.kaleid-backup/<时间戳>/...`）。
fn backup_files(mods_dir: &Path) -> Vec<String> {
    let root = mods_dir.join(".kaleid-backup");
    let mut out = Vec::new();
    let Ok(stamps) = std::fs::read_dir(&root) else {
        return out;
    };
    for stamp in stamps.flatten() {
        let Ok(files) = std::fs::read_dir(stamp.path()) else {
            continue;
        };
        for file in files.flatten() {
            out.push(file.file_name().to_string_lossy().to_string());
        }
    }
    out.sort();
    out
}

/// `mods/` 里是否还有下载器的临时残留。
fn has_staging_leftover(mods_dir: &Path) -> bool {
    std::fs::read_dir(mods_dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".kaleid-update-")
            })
        })
        .unwrap_or(false)
}

#[test]
fn diagnostics_update_failure_after_download_is_not_operation_success() {
    let body = b"new jar bytes".to_vec();
    let server = FileServer::start(body.clone());
    let mut f = fixture();
    let kernel = kaleid_kernel::Kernel::new();
    let records = Arc::new(std::sync::Mutex::new(Vec::<kaleid_kernel::DiagnosticRecord>::new()));
    let captured = records.clone();
    kernel.context().on::<kaleid_kernel::DiagnosticRecord>(move |r| captured.lock().unwrap().push(r.clone()));
    f.svc = f.svc.with_context(kernel.context().clone());
    std::fs::write(f.mods_dir.join("sodium-0.6.0.jar"), b"old").unwrap();
    // Download succeeds, but a regular file blocks creation of the backup directory.
    std::fs::write(f.mods_dir.join(".kaleid-backup"), b"blocked").unwrap();
    let update = update_for(format!("{}/secret?token=SECRET_QUERY", server.base), body.len() as u64, "new.jar");
    assert!(f.svc.apply_update(&f.root_id, "1.21.4-fabric", &update).is_err());
    let records = records.lock().unwrap();
    let ended: Vec<_> = records.iter().filter(|r| r.event == "operation.finished").collect();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].fields["operation"], "mod_update");
    assert_eq!(ended[0].fields["outcome"], "failed");
    assert_eq!(ended[0].fields["category"], "io");
    assert!(!serde_json::to_string(&*records).unwrap().contains("SECRET_QUERY"));
}

#[test]
fn diagnostics_update_success_persists_safe_lifecycle_to_file_sink() {
    let body = b"new jar bytes".to_vec();
    let server = FileServer::start(body.clone());
    let mut f = fixture();
    let kernel = kaleid_kernel::Kernel::new();
    let logger = kaleid_core_plugins::bootstrap_logger(&kernel, &f._dir.path().join("launcher-data"));
    f.svc = f.svc.with_context(kernel.context().clone());
    std::fs::write(f.mods_dir.join("sodium-0.6.0.jar"), b"old").unwrap();
    let update = update_for(format!("{}/secret?token=SECRET_QUERY", server.base), body.len() as u64, "new.jar");
    f.svc.apply_update(&f.root_id, "1.21.4-fabric", &update).unwrap();
    assert!(matches!(logger.flush(), kaleid_core_plugins::LoggerControlOutcome::Flushed));
    let text = std::fs::read_to_string(logger.run_dir().join("launcher.log")).unwrap();
    let records: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let ended: Vec<_> = records.iter().filter(|r| r["event"] == "operation.finished").collect();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0]["fields"]["outcome"], "succeeded");
    assert_eq!(ended[0]["fields"]["project_id"], "AANobbMI");
    assert_eq!(ended[0]["fields"]["root_id"], f.root_id, "preserve established root correlation on the domain result");
    assert!(records.iter().filter(|r| r["event"] == "operation.accepted").all(|r| r["fields"].get("root_id").is_none()));
    assert!(!text.contains("SECRET_QUERY"));
}

#[test]
fn apply_update_replaces_file_and_keeps_a_backup() {
    let body = b"new jar bytes".to_vec();
    let server = FileServer::start(body.clone());
    let f = fixture();
    std::fs::write(f.mods_dir.join("sodium-0.6.0.jar"), b"old jar").unwrap();

    let update = update_for(
        format!("{}/sodium-0.6.9.jar", server.base),
        body.len() as u64,
        "sodium-0.6.9.jar",
    );
    f.svc
        .apply_update(&f.root_id, "1.21.4-fabric", &update)
        .unwrap();

    // 新文件就位、内容正确
    assert_eq!(
        std::fs::read(f.mods_dir.join("sodium-0.6.9.jar")).unwrap(),
        body
    );
    // 旧文件没被删掉，只是搬进了备份目录
    assert!(!f.mods_dir.join("sodium-0.6.0.jar").exists());
    assert_eq!(backup_files(&f.mods_dir), vec!["sodium-0.6.0.jar".to_string()]);
    // 没有临时文件残留
    assert!(!has_staging_leftover(&f.mods_dir));
}

#[test]
fn apply_update_keeps_the_disabled_state_of_the_old_file() {
    let body = b"new jar bytes".to_vec();
    let server = FileServer::start(body.clone());
    let f = fixture();
    // 旧文件是停用态：更新后也应当保持停用
    std::fs::write(f.mods_dir.join("sodium-0.6.0.jar.disabled"), b"old jar").unwrap();

    let mut update = update_for(
        format!("{}/sodium-0.6.9.jar", server.base),
        body.len() as u64,
        "sodium-0.6.9.jar",
    );
    update.file_name = "sodium-0.6.0.jar.disabled".into();
    f.svc
        .apply_update(&f.root_id, "1.21.4-fabric", &update)
        .unwrap();

    assert!(f.mods_dir.join("sodium-0.6.9.jar.disabled").exists());
    assert!(!f.mods_dir.join("sodium-0.6.9.jar").exists());
    assert_eq!(
        backup_files(&f.mods_dir),
        vec!["sodium-0.6.0.jar.disabled".to_string()]
    );
}

#[test]
fn apply_update_never_overwrites_an_existing_file() {
    let body = b"new jar bytes".to_vec();
    let server = FileServer::start(body.clone());
    let f = fixture();
    std::fs::write(f.mods_dir.join("sodium-0.6.0.jar"), b"old jar").unwrap();
    // 用户手里已经有个同名的新文件：不能覆盖它
    std::fs::write(f.mods_dir.join("sodium-0.6.9.jar"), b"user copy").unwrap();

    let update = update_for(
        format!("{}/sodium-0.6.9.jar", server.base),
        body.len() as u64,
        "sodium-0.6.9.jar",
    );
    f.svc
        .apply_update(&f.root_id, "1.21.4-fabric", &update)
        .unwrap();

    assert_eq!(
        std::fs::read(f.mods_dir.join("sodium-0.6.9.jar")).unwrap(),
        b"user copy"
    );
    assert_eq!(
        std::fs::read(f.mods_dir.join("sodium-0.6.9 (1).jar")).unwrap(),
        body
    );
}

#[test]
fn apply_update_failure_leaves_the_old_file_alone() {
    // 服务器端口没人监听：下载必然失败
    let f = fixture();
    std::fs::write(f.mods_dir.join("sodium-0.6.0.jar"), b"old jar").unwrap();
    let update = update_for("http://127.0.0.1:1/sodium-0.6.9.jar".into(), 10, "sodium-0.6.9.jar");
    let err = f
        .svc
        .apply_update(&f.root_id, "1.21.4-fabric", &update)
        .unwrap_err();
    assert!(!err.to_string().is_empty(), "应当给出中文错误");

    // 旧文件还在原处，没有半截新文件，也没有备份残留
    assert_eq!(
        std::fs::read(f.mods_dir.join("sodium-0.6.0.jar")).unwrap(),
        b"old jar"
    );
    assert!(!f.mods_dir.join("sodium-0.6.9.jar").exists());
    assert!(backup_files(&f.mods_dir).is_empty());
    assert!(!has_staging_leftover(&f.mods_dir));
}

#[test]
fn apply_update_without_downloader_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let game = dir.path().join(".minecraft");
    std::fs::create_dir_all(&game).unwrap();
    let roots = Arc::new(RootRegistry::empty_at(&data).unwrap());
    let root = roots.add(&game, None, true).unwrap();
    let settings = Arc::new(Settings::open(&data));
    // 没有挂下载器（=「版本管理」插件被停用）
    let svc = ContentService::new(settings, roots.clone(), Arc::new(ModNames::default()));
    let err = svc
        .apply_update(
            &root.id,
            "1.21.4",
            &update_for("http://127.0.0.1:1/x.jar".into(), 1, "x.jar"),
        )
        .unwrap_err();
    assert!(err.to_string().contains("下载服务不可用"), "got {err}");
}

/// 联网冒烟：用一个假 jar 走一遍真实的「查哈希 / 查指纹」两个接口。
///
/// 目的是确认请求体形状没写错（写错了站点会回 4xx，而单元测试里看不出来）。
/// 假 jar 当然对不上任何项目，所以期望结果是 `unknown`、无错误。
#[test]
#[ignore = "需要联网；手动 cargo test -- --ignored 执行"]
fn update_check_reaches_both_sites_without_matching() {
    let f = fixture();
    std::fs::write(f.mods_dir.join("not-a-real-mod-1.0.jar"), b"nope").unwrap();

    let report = f
        .svc
        .check_updates(&f.root_id, "1.21.4-fabric")
        .expect("更新检查不该整次失败");
    assert!(report.updates.is_empty(), "{report:?}");
    assert_eq!(report.unknown, vec!["not-a-real-mod-1.0.jar".to_string()]);
    // 请求发不出去（例如站点抽风）时会带 warnings；这里只要求不是 panic/错
    println!("warnings = {:?}", report.warnings);
}

/// 联网冒烟：装一个**真的**最新 jar，检查结果应当是"认得出来、但没有更新"。
///
/// 这一条覆盖了整条链路：算 SHA-1 → Modrinth 认文件 → 查该项目当前版本下的最新
/// 文件 → 判断有没有更新。前两步任一处解析写错都会在这一步暴露。
#[test]
#[ignore = "需要联网；手动 cargo test -- --ignored 执行"]
fn update_check_recognizes_a_real_up_to_date_mod() {
    // Sodium（Modrinth id: AANobbMI）的 1.21.4 + fabric 版本清单
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent("KMLH-test/0.0.0")
        .build();
    let url = "https://api.modrinth.com/v2/project/AANobbMI/version?limit=50&game_versions=%5B%221.21.4%22%5D&loaders=%5B%22fabric%22%5D";
    let versions: serde_json::Value = agent
        .get(url)
        .call()
        .expect("应当能访问 Modrinth")
        .into_json()
        .expect("应当返回 JSON");
    let newest = versions
        .as_array()
        .and_then(|list| list.first())
        .expect("应当至少有一个版本");
    let file = newest
        .get("files")
        .and_then(|v| v.as_array())
        .and_then(|files| {
            files
                .iter()
                .find(|f| f.get("primary").and_then(|v| v.as_bool()).unwrap_or(false))
                .or_else(|| files.first())
        })
        .expect("版本里应当有文件");
    let file_name = file["filename"].as_str().unwrap().to_string();
    let download = file["url"].as_str().unwrap().to_string();
    let bytes = agent
        .get(&download)
        .call()
        .expect("应当能下载 jar")
        .into_reader()
        .bytes()
        .collect::<Result<Vec<u8>, _>>()
        .expect("应当读到 jar 内容");

    let f = fixture();
    std::fs::write(f.mods_dir.join(&file_name), &bytes).unwrap();
    let report = f
        .svc
        .check_updates(&f.root_id, "1.21.4-fabric")
        .expect("更新检查不该整次失败");
    println!("{report:?}");

    // 装的就是最新版：不该报更新，也不该算"认不出来"
    assert!(report.updates.is_empty(), "{report:?}");
    assert!(report.unknown.is_empty(), "{report:?}");
    assert!(report.warnings.is_empty(), "{report:?}");

    // 再把最老的那一版装上：应当恰好报一条更新，且指向最新文件
    let oldest = versions
        .as_array()
        .and_then(|list| list.last())
        .expect("应当有一个最老的版本");
    let old_file = oldest["files"]
        .as_array()
        .and_then(|files| {
            files
                .iter()
                .find(|f| f.get("primary").and_then(|v| v.as_bool()).unwrap_or(false))
                .or_else(|| files.first())
        })
        .expect("旧版本里也应当有文件");
    let old_bytes = agent
        .get(old_file["url"].as_str().unwrap())
        .call()
        .expect("应当能下载旧 jar")
        .into_reader()
        .bytes()
        .collect::<Result<Vec<u8>, _>>()
        .expect("应当读到旧 jar 内容");
    std::fs::remove_file(f.mods_dir.join(&file_name)).unwrap();
    let old_name = old_file["filename"].as_str().unwrap().to_string();
    std::fs::write(f.mods_dir.join(&old_name), &old_bytes).unwrap();

    let report = f
        .svc
        .check_updates(&f.root_id, "1.21.4-fabric")
        .expect("更新检查不该整次失败");
    println!("{report:?}");
    assert_eq!(report.updates.len(), 1, "{report:?}");
    let update = &report.updates[0];
    assert_eq!(update.file_name, old_name);
    assert_eq!(update.source, "modrinth");
    assert_eq!(update.project_id, "AANobbMI");
    assert_eq!(update.latest_file_name, file_name);
    assert!(!update.download_url.is_empty(), "{update:?}");
    assert!(update.sha1.as_deref() == file["hashes"]["sha1"].as_str(), "{update:?}");
}
