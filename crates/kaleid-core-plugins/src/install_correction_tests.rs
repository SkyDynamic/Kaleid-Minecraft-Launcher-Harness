// Included in install::tests: exercise actual preflight and startup recovery decisions.

struct CorrectionHttp {
    url: String,
    hits: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CorrectionHttp {
    fn serve(body: &'static [u8]) -> Self {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/index.json", listener.local_addr().unwrap());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (counter, stopping) = (hits.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let _ = stream.read(&mut [0u8; 4096]);
                        counter.fetch_add(1, Ordering::SeqCst);
                        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("local HTTP fixture: {error}"),
                }
            }
        });
        Self {
            url,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for CorrectionHttp {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn correction_asset_index_rejects_bad_candidates_before_accepting_a_source() {
    let good_bytes = br#"{"objects":{}}"#;
    let digest = hex::encode(Sha1::digest(good_bytes));
    for (bad_bytes, expected_size, expected_sha1) in [
        (
            &br#"{"objects":[]}"#[..],
            good_bytes.len() as u64,
            Some(digest.as_str()),
        ),
        (
            &br#"{"objects":{},"extra":true}"#[..],
            good_bytes.len() as u64,
            None,
        ),
        (&b"{ truncated"[..], 0, None),
    ] {
        let bad = CorrectionHttp::serve(bad_bytes);
        let good = CorrectionHttp::serve(good_bytes);
        let (meta, value) = fetch_asset_index(
            &RemoteClient::new(),
            &[bad.url.clone(), good.url.clone()],
            "fixture",
            expected_size,
            expected_sha1,
            crate::remote::VERIFY_MAX_BYTES,
        )
        .expect("bad index content must fall back to a later healthy source");
        assert_eq!(meta.bytes, good_bytes);
        assert!(value["objects"].is_object());
        assert_eq!(bad.hits(), 1, "bad content is not fetched twice");
        assert_eq!(good.hits(), 1, "accepted index is fetched only once");
    }
}

#[test]
fn correction_asset_index_is_verified_written_and_reused_without_refetching() {
    let (_tmp, root, service) = correction_service_with_root();
    let bytes = br#"{"objects":{}}"#;
    let server = CorrectionHttp::serve(bytes);
    let value = json!({"assetIndex": {"id": "fixture", "url": server.url,
        "size": bytes.len(), "sha1": hex::encode(Sha1::digest(bytes))}});
    let job = Job::new(
        "index-job".into(),
        "fixture".into(),
        "fixture".into(),
        root.id.clone(),
        "1.20.1".into(),
        LoaderKind::Vanilla,
        None,
        "official-only".into(),
    );
    for _ in 0..2 {
        assert!(service
            .vanilla_asset_items(&job, &root, "1.20.1", &value, "official-only")
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        std::fs::read(Path::new(&root.path).join("assets/indexes/fixture.json")).unwrap(),
        bytes
    );
    assert_eq!(server.hits(), 1);
}

fn correction_completion_fixture() -> (
    tempfile::TempDir,
    PathBuf,
    InstallRequest,
    Arc<Job>,
    InstallGuard,
) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("versions/fixture");
    let mut guard = InstallGuard::new();
    guard.track(&dir);
    std::fs::create_dir_all(dir.join("mods")).unwrap();
    write_install_marker(&dir, "completion-job").unwrap();
    let req = InstallRequest {
        root_id: "r".into(),
        game_version: "1.20.1".into(),
        version_name: "fixture".into(),
        loader: InstallLoader::Vanilla,
        install_fabric_api: false,
        optifine: None,
    };
    let job = Job::new(
        "completion-job".into(),
        "fixture".into(),
        "fixture".into(),
        "r".into(),
        "1.20.1".into(),
        LoaderKind::Vanilla,
        None,
        "official-only".into(),
    );
    (tmp, dir, req, job, guard)
}

fn correction_staged_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join("mods").join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn correction_publication_success_completes_only_with_required_mod_present() {
    let (tmp, dir, req, job, guard) = correction_completion_fixture();
    let from = correction_staged_file(&dir, "api.jar", b"required mod");
    let to = tmp.path().join("mods/api.jar");
    finish_install(
        &job,
        &dir,
        &req,
        &json!({"id":"fixture"}),
        vec![(from, to.clone())],
        guard,
    )
    .unwrap();
    assert_eq!(std::fs::read(to).unwrap(), b"required mod");
    assert!(dir.join("fixture.json").is_file());
    assert!(!dir.join(INSTALL_MARKER).exists());
    assert_eq!(job.snapshot().status, kaleid_api::DownloadStatus::Completed);
}

#[test]
fn correction_publication_obstruction_errors_without_publishing_version() {
    let (tmp, dir, req, job, guard) = correction_completion_fixture();
    let from = correction_staged_file(&dir, "api.jar", b"required mod");
    let to = tmp.path().join("mods/api.jar");
    std::fs::create_dir_all(&to).unwrap();
    let result = finish_install(
        &job,
        &dir,
        &req,
        &json!({"id":"fixture"}),
        vec![(from, to.clone())],
        guard,
    );
    assert!(
        result.is_err(),
        "required publication errors must not be discarded"
    );
    assert_ne!(job.snapshot().status, kaleid_api::DownloadStatus::Completed);
    assert!(!dir.exists(), "unpublished version must roll back");
    assert!(to.is_dir(), "preexisting obstruction is untouched");
}

#[test]
fn correction_publication_differing_preexisting_mod_is_untouched() {
    let (tmp, dir, req, job, guard) = correction_completion_fixture();
    let from = correction_staged_file(&dir, "api.jar", b"new mod");
    let to = tmp.path().join("mods/api.jar");
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::write(&to, b"preexisting mod").unwrap();
    let result = finish_install(
        &job,
        &dir,
        &req,
        &json!({"id":"fixture"}),
        vec![(from, to.clone())],
        guard,
    );
    assert!(
        result.is_err(),
        "a differing target is a conflict, not an overwrite"
    );
    assert_eq!(std::fs::read(to).unwrap(), b"preexisting mod");
    assert_ne!(job.snapshot().status, kaleid_api::DownloadStatus::Completed);
    assert!(!dir.exists());
}

#[test]
fn correction_publication_identical_preexisting_mod_is_reused() {
    let (tmp, dir, req, job, guard) = correction_completion_fixture();
    let from = correction_staged_file(&dir, "api.jar", b"same mod");
    let to = tmp.path().join("mods/api.jar");
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::write(&to, b"same mod").unwrap();
    finish_install(
        &job,
        &dir,
        &req,
        &json!({"id":"fixture"}),
        vec![(from, to.clone())],
        guard,
    )
    .unwrap();
    assert_eq!(std::fs::read(to).unwrap(), b"same mod");
    assert_eq!(job.snapshot().status, kaleid_api::DownloadStatus::Completed);
}

#[test]
fn correction_publication_later_conflict_rolls_back_only_new_shared_files() {
    let (tmp, dir, req, job, guard) = correction_completion_fixture();
    let new = tmp.path().join("mods/new.jar");
    let same = tmp.path().join("mods/same.jar");
    let conflict = tmp.path().join("mods/conflict.jar");
    std::fs::create_dir_all(same.parent().unwrap()).unwrap();
    std::fs::write(&same, b"same mod").unwrap();
    std::fs::write(&conflict, b"user mod").unwrap();
    let staged = vec![
        (
            correction_staged_file(&dir, "new.jar", b"new mod"),
            new.clone(),
        ),
        (
            correction_staged_file(&dir, "same.jar", b"same mod"),
            same.clone(),
        ),
        (
            correction_staged_file(&dir, "conflict.jar", b"different mod"),
            conflict.clone(),
        ),
    ];
    assert!(finish_install(&job, &dir, &req, &json!({"id":"fixture"}), staged, guard).is_err());
    assert!(!new.exists());
    assert_eq!(std::fs::read(same).unwrap(), b"same mod");
    assert_eq!(std::fs::read(conflict).unwrap(), b"user mod");
    assert!(!dir.exists());
    assert_ne!(job.snapshot().status, kaleid_api::DownloadStatus::Completed);
}

#[test]
fn correction_publication_finalization_failure_rolls_back_only_new_shared_files() {
    let (tmp, dir, req, job, guard) = correction_completion_fixture();
    let new = tmp.path().join("mods/new.jar");
    let same = tmp.path().join("mods/same.jar");
    std::fs::create_dir_all(same.parent().unwrap()).unwrap();
    std::fs::write(&same, b"same mod").unwrap();
    // Portable obstruction in the actual final version JSON publication step.
    std::fs::create_dir_all(dir.join("fixture.json")).unwrap();
    let staged = vec![
        (
            correction_staged_file(&dir, "new.jar", b"new mod"),
            new.clone(),
        ),
        (
            correction_staged_file(&dir, "same.jar", b"same mod"),
            same.clone(),
        ),
    ];
    assert!(finish_install(&job, &dir, &req, &json!({"id":"fixture"}), staged, guard).is_err());
    assert!(!new.exists());
    assert_eq!(std::fs::read(same).unwrap(), b"same mod");
    assert!(!dir.exists());
    assert_ne!(job.snapshot().status, kaleid_api::DownloadStatus::Completed);
}

fn correction_service_with_root() -> (tempfile::TempDir, Root, InstallService) {
    let (tmp, _root, mut service) = service_with_root();
    let game = tmp.path().join(".minecraft");
    std::fs::create_dir_all(&game).unwrap();
    service.roots = Arc::new(RootRegistry::empty_at(tmp.path()).unwrap());
    let root = service.roots.add(&game, None, true).unwrap();
    (tmp, root, service)
}

#[test]
fn correction_preflight_preserves_foreign_alternate_json_and_user_data() {
    let (_tmp, root, service) = correction_service_with_root();
    let dir = loader_version_dir(&root, "foreign");
    std::fs::create_dir_all(dir.join("mods")).unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("neoforge-21.1.72.json"), br#"{"id":"foreign"}"#).unwrap();
    std::fs::write(dir.join("mods/user.jar"), b"user mod").unwrap();
    std::fs::write(dir.join("config/user.toml"), b"user config").unwrap();
    assert!(service.validate_and_check(&root, "foreign").is_err());
    sweep_incomplete_installs(service.roots.as_ref());
    assert_eq!(
        std::fs::read(dir.join("mods/user.jar")).unwrap(),
        b"user mod"
    );
    assert_eq!(
        std::fs::read(dir.join("config/user.toml")).unwrap(),
        b"user config"
    );
}

#[test]
fn correction_preflight_rejects_unmarked_existing_folder() {
    let (_tmp, root, service) = correction_service_with_root();
    let dir = loader_version_dir(&root, "unmarked");
    std::fs::create_dir_all(dir.join("libraries")).unwrap();
    assert!(service.validate_and_check(&root, "unmarked").is_err());
    sweep_incomplete_installs(service.roots.as_ref());
    assert!(dir.join("libraries").is_dir());
}

#[test]
fn correction_stale_marker_does_not_override_any_existing_json() {
    let (_tmp, root, service) = correction_service_with_root();
    for (name, json_name, contents) in [
        ("complete", "complete.json", r#"{"id":"complete"}"#),
        (
            "alternate",
            "neoforge-21.1.72.JSON",
            r#"{"id":"alternate"}"#,
        ),
        ("damaged", "damaged.json", "{ truncated"),
    ] {
        let dir = loader_version_dir(&root, name);
        std::fs::create_dir_all(&dir).unwrap();
        write_install_marker(&dir, "stale-job").unwrap();
        std::fs::write(dir.join(json_name), contents).unwrap();
        assert!(service.validate_and_check(&root, name).is_err(), "{name}");
    }
    sweep_incomplete_installs(service.roots.as_ref());
    for (name, json_name, contents) in [
        ("complete", "complete.json", r#"{"id":"complete"}"#),
        (
            "alternate",
            "neoforge-21.1.72.JSON",
            r#"{"id":"alternate"}"#,
        ),
        ("damaged", "damaged.json", "{ truncated"),
    ] {
        let dir = loader_version_dir(&root, name);
        assert_eq!(
            std::fs::read_to_string(dir.join(json_name)).unwrap(),
            contents
        );
        assert!(!dir.join(INSTALL_MARKER).exists());
    }
}

#[test]
fn correction_stale_marker_keeps_user_data_and_unknown_files() {
    let (_tmp, root, service) = correction_service_with_root();
    for user_path in [
        "mods/user.jar",
        "config/user.toml",
        "saves/world/level.dat",
        "resourcepacks/user.zip",
        "keep.txt",
    ] {
        let name = user_path.replace(['/', '.'], "-");
        let dir = loader_version_dir(&root, &name);
        let file = dir.join(user_path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"user bytes").unwrap();
        write_install_marker(&dir, "stale-job").unwrap();
        assert!(
            service.validate_and_check(&root, &name).is_err(),
            "{user_path}"
        );
    }
    sweep_incomplete_installs(service.roots.as_ref());
    for user_path in [
        "mods/user.jar",
        "config/user.toml",
        "saves/world/level.dat",
        "resourcepacks/user.zip",
        "keep.txt",
    ] {
        let name = user_path.replace(['/', '.'], "-");
        assert_eq!(
            std::fs::read(loader_version_dir(&root, &name).join(user_path)).unwrap(),
            b"user bytes"
        );
    }
}

#[test]
fn correction_sweeper_preserves_marked_alternate_json_and_user_data() {
    let (_tmp, root, service) = correction_service_with_root();
    for (name, file) in [
        ("alternate", "neoforge.json"),
        ("damaged", "damaged.json"),
        ("mods", "mods/user.jar"),
        ("config", "config/user.toml"),
    ] {
        let dir = loader_version_dir(&root, name);
        let target = dir.join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"existing bytes").unwrap();
        write_install_marker(&dir, "stale-job").unwrap();
    }
    sweep_incomplete_installs(service.roots.as_ref());
    for (name, file) in [
        ("alternate", "neoforge.json"),
        ("damaged", "damaged.json"),
        ("mods", "mods/user.jar"),
        ("config", "config/user.toml"),
    ] {
        assert_eq!(
            std::fs::read(loader_version_dir(&root, name).join(file)).unwrap(),
            b"existing bytes",
            "{name}"
        );
    }
}

#[test]
fn correction_owned_partial_allows_retry_and_startup_cleanup() {
    let (_tmp, root, service) = correction_service_with_root();
    let dir = loader_version_dir(&root, "partial");
    std::fs::create_dir_all(dir.join("libraries")).unwrap();
    std::fs::write(dir.join("partial.jar"), b"partial jar").unwrap();
    write_install_marker(&dir, "our-job").unwrap();
    assert!(service.validate_and_check(&root, "partial").is_ok());
    sweep_incomplete_installs(service.roots.as_ref());
    assert!(!dir.exists());
}
