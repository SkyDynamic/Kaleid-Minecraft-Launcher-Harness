include!("mods_fix1_diagnostics_tests.rs");

fn diagnostic_mod_fixture(
    url: &str,
    body: &[u8],
) -> (tempfile::TempDir, ModService, ModInstallRequest, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let roots = Arc::new(RootRegistry::empty_at(dir.path()).unwrap());
    let mc = dir.path().join("mc");
    std::fs::create_dir_all(mc.join("versions/fixture")).unwrap();
    std::fs::write(mc.join("versions/fixture/fixture.json"), json!({"id":"fixture","mainClass":"net.minecraft.client.main.Main","kaleidBaseGameVersion":"1.20.1","libraries":[]}).to_string()).unwrap();
    let root = roots.add(&mc, None, true).unwrap();
    let settings = Arc::new(Settings::open(dir.path()));
    let downloads = Arc::new(DownloadManager::new(settings.clone()));
    let service = ModService::new(
        settings,
        roots,
        Arc::new(VersionStore::new()),
        Some(downloads),
        dir.path(),
    );
    let file = ModFile {
        source: ModSource::Modrinth,
        project_id: "project".into(),
        file_id: "file".into(),
        name: "SECRET_NAME".into(),
        file_name: "fixture.jar".into(),
        version_number: "1".into(),
        size: body.len() as u64,
        downloads: 0,
        published_at: String::new(),
        release_type: "release".into(),
        loaders: vec![],
        game_versions: vec!["1.20.1".into()],
        official_url: url.into(),
        mirror_url: None,
        sha1: Some(hex::encode(Sha1::digest(body))),
        required_deps: 0,
    };
    service.detail.put(
        "modrinth:project".into(),
        ModDetail {
            project: build_project_from_modrinth(
                &json!({"id":"project","slug":"fixture","title":"SECRET_TITLE"}),
                &[],
            ),
            groups: vec![ModVersionGroup {
                loader: "vanilla".into(),
                loader_label: String::new(),
                game_version: "1.20".into(),
                dependencies: vec![],
                optional_dependencies: vec![],
                files: vec![file],
            }],
            versions: vec![],
            warnings: vec![],
        },
    );
    let req = ModInstallRequest {
        root_id: root.id,
        version_id: "fixture".into(),
        source: ModSource::Modrinth,
        project_id: "project".into(),
        file_id: "file".into(),
        overwrite: false,
    };
    let dest = PathBuf::from(
        service
            .target(&req.root_id, &req.version_id)
            .unwrap()
            .mods_dir,
    )
    .join("fixture.jar");
    (dir, service, req, dest)
}

#[test]
fn diagnostics_mod_identical_shortcut_and_async_download_finish_once() {
    for existing in [true, false] {
        let body = b"fixture mod bytes";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/secret?token=SECRET_QUERY",
            listener.local_addr().unwrap()
        );
        let (_dir, service, req, dest) = diagnostic_mod_fixture(&url, body);
        let kernel = kaleid_kernel::Kernel::new();
        let records = Arc::new(Mutex::new(Vec::<kaleid_kernel::DiagnosticRecord>::new()));
        let captured = records.clone();
        kernel
            .context()
            .on::<kaleid_kernel::DiagnosticRecord>(move |r| {
                if let Ok(mut records) = captured.lock() {
                    records.push(r.clone());
                }
            });
        let service = service.with_context(kernel.context().clone());
        let server = if existing {
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(&dest, body).unwrap();
            None
        } else {
            Some(std::thread::spawn(move || {
                use std::io::{Read, Write};
                listener.set_nonblocking(true).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Ok((mut stream, _)) = listener.accept() {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0; 4096];
                        let _ = stream.read(&mut request);
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .unwrap();
                        stream.write_all(body).unwrap();
                        break;
                    }
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }))
        };
        let queued = service.install(&req).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let captured = records.lock().unwrap().clone();
            let ended: Vec<_> = captured
                .iter()
                .filter(|r| r.event == "operation.finished")
                .collect();
            if !ended.is_empty() {
                assert_eq!(ended.len(), 1);
                assert_eq!(ended[0].fields["outcome"], "succeeded");
                assert_eq!(ended[0].job_id.as_deref(), Some(queued.job_id.as_str()));
                assert_eq!(ended[0].fields["project_id"], "project");
                assert_eq!(ended[0].fields["file_id"], "file");
                assert_eq!(ended[0].fields["root_id"], req.root_id, "retain the resolved target root on the domain result");
                assert!(captured.iter().filter(|r| r.event == "operation.accepted").all(|r| !r.fields.contains_key("root_id")));
                if existing {
                    assert_eq!(ended[0].fields["reason"], "existing_identical");
                }
                assert!(!serde_json::to_string(&captured).unwrap().contains("SECRET"));
                assert_eq!(std::fs::read(&dest).unwrap(), body);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(server) = server {
            server.join().unwrap();
        }
    }
}
