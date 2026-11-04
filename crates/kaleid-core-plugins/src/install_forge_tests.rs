// Included in install::tests so these exercise the real private dependency planner.

#[test]
#[ignore = "downloads a full Minecraft/Forge installation and runs a local Java installer"]
fn real_forge_1_16_5_installs_with_bundled_libraries() {
    real_forge_install("1.16.5", "36.2.42", 8);
}

#[test]
#[ignore = "downloads a full Minecraft/Forge installation and runs a local Java installer"]
fn real_forge_1_20_1_installs_with_remote_libraries() {
    real_forge_install("1.20.1", "47.2.0", 17);
}

fn real_forge_install(game: &str, forge: &str, java_major: u32) {
    let (tmp, mut root, service) = service_with_root();
    root.path = tmp.path().join("minecraft").to_string_lossy().to_string();
    let java = crate::java::JavaService::new(crate::java::ScanEnv::current(
        Vec::new(),
        std::env::var_os("KALEID_JAVA").map(PathBuf::from),
    ));
    let service = service.with_java(Some(Arc::new(java)));
    if let Ok(explicit) = std::env::var("KALEID_JAVA") {
        service
            .settings
            .patch_global(&json!({"javaPath": explicit}))
            .unwrap();
    }
    let java_path = service
        .installer_java(&json!({"javaVersion": {"majorVersion": java_major}}))
        .expect(
            "install a suitable Java runtime or set KALEID_JAVA before running this opt-in test",
        );
    service
        .settings
        .patch_global(&json!({
            "javaPath": java_path, "downloadSource": "mirror-first", "downloadConcurrency": 8
        }))
        .unwrap();
    let name = format!("{game}-Forge-{forge}-smoke");
    let req = InstallRequest {
        root_id: root.id.clone(),
        game_version: game.into(),
        version_name: name.clone(),
        loader: InstallLoader::Forge {
            version: forge.into(),
        },
        install_fabric_api: false,
        optifine: None,
    };
    let job = Job::new(
        format!("forge-smoke-{game}"),
        name.clone(),
        name.clone(),
        root.id.clone(),
        game.into(),
        LoaderKind::Forge,
        Some(forge.into()),
        "mirror-first".into(),
    );
    eprintln!(
        "Installing {game} / Forge {forge} in {} using {java_path}",
        tmp.path().display()
    );
    let result = std::thread::scope(|scope| {
        let progress_job = job.clone();
        let (finished, progress) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            for tick in 1..=20 {
                match progress.recv_timeout(Duration::from_secs(30)) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                let snapshot = progress_job.snapshot();
                eprintln!(
                    "{game} / Forge {forge}: {} files, {} / {} bytes; {}",
                    snapshot.files_done,
                    snapshot.done_bytes,
                    snapshot.total_bytes,
                    snapshot.current
                );
                if tick == 20 {
                    eprintln!(
                        "{game} / Forge {forge}: cancelling after the 10-minute smoke-test budget"
                    );
                    progress_job.cancel();
                }
            }
        });
        let result = service.plan_and_run(&job, &root, &req);
        drop(finished);
        result
    });
    result.unwrap_or_else(|error| panic!("{game} / Forge {forge} installation failed: {error}"));
    assert_eq!(job.snapshot().status, kaleid_api::DownloadStatus::Completed);
    let version_dir = loader_version_dir(&root, &name);
    let installed: Value =
        serde_json::from_slice(&std::fs::read(version_dir.join(format!("{name}.json"))).unwrap())
            .unwrap();
    assert_eq!(installed["id"], name);
    assert!(
        installed.get("inheritsFrom").is_none(),
        "result must be self-contained"
    );
    assert!(version_dir.join(format!("{name}.jar")).is_file());
    assert!(!version_dir.join(".kaleid-incomplete").exists());
    let mut forge_jars = 0;
    for lib in installed["libraries"].as_array().unwrap() {
        if !lib["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("net.minecraftforge:")
        {
            continue;
        }
        let path = library_relative_path(lib).expect("Forge runtime artifact path");
        let artifact = &lib["downloads"]["artifact"];
        let sha1 = artifact["sha1"]
            .as_str()
            .filter(|value| valid_sha1(value).is_some())
            .expect("official Forge runtime metadata must provide a valid SHA-1");
        let size = artifact["size"]
            .as_u64()
            .filter(|size| *size > 0)
            .expect("official Forge runtime metadata must provide a positive size");
        assert!(
            file_already_good(&version_dir.join("libraries").join(&path), Some(sha1), size,),
            "{game}: Forge artifact missing or checksum/size mismatch: {path}"
        );
        forge_jars += 1;
    }
    assert!(
        forge_jars > 0,
        "installed profile must contain Forge runtime artifacts"
    );
    if game == "1.20.1" {
        assert!(file_already_good(
            &version_dir.join("libraries/net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-universal.jar"),
            Some("ac078f159add92ef9dce7a8259508c9d78470d3a"), 2672123,
        ), "remote Forge universal JAR must match the official installer metadata");
    }
    if game == "1.16.5" {
        for (path, sha1, size) in [
            (
                "net/minecraftforge/forge/1.16.5-36.2.42/forge-1.16.5-36.2.42.jar",
                "caca08d22543cbe101d64d171738e1ff6eef4de3",
                212563,
            ),
            (
                "net/minecraftforge/forge/1.16.5-36.2.42/forge-1.16.5-36.2.42-universal.jar",
                "fd07fe6c2c1cc7d3e403a1db7eadfa935ca7640d",
                2540080,
            ),
        ] {
            assert!(
                file_already_good(&version_dir.join("libraries").join(path), Some(sha1), size),
                "installer must extract its bundled artifact unchanged: {path}"
            );
        }
    }
    eprintln!(
        "PASS {game} / Forge {forge}: completed, {forge_jars} runtime Forge JAR(s) validated"
    );
}

#[test]
fn installer_prefetch_leaves_bundled_and_generated_artifacts_to_the_installer() {
    let (tmp, _root, service) = service_with_root();
    let staging = tmp.path().join("staging");
    std::fs::create_dir_all(&staging).unwrap();
    let profile = json!({"libraries": [{
        "name": "example:bundled:1",
        "downloads": {"artifact": {"path": "example/bundled/1/bundled-1.jar", "url": ""}}
    }]});
    let version = json!({"libraries": [{
        "name": "example:generated:1",
        "downloads": {"artifact": {"path": "example/generated/1/generated-1.jar", "url": "\t "}}
    }]});
    // Fail fast if this regresses instead of making the default suite try public mirrors.
    assert!(installer_library_items(&profile, &version, &staging.join("libraries")).is_empty());
    let profile_bytes = serde_json::to_vec(&profile).unwrap();
    let version_bytes = serde_json::to_vec(&version).unwrap();
    let jar = crate::jar::tests::zip_stored(&[
        ("install_profile.json", &profile_bytes),
        ("version.json", &version_bytes),
        ("maven/example/bundled/1/bundled-1.jar", b"bundled bytes"),
    ]);
    std::fs::write(staging.join("installer.jar"), &jar).unwrap();
    let job = Job::new(
        "bundled-only".into(),
        "fixture".into(),
        "fixture".into(),
        "r".into(),
        "fixture".into(),
        LoaderKind::Forge,
        None,
        "official-only".into(),
    );
    service
        .prefetch_installer_libraries(&job, &staging, "Forge", &Value::Null)
        .unwrap();
    assert_eq!(
        job.snapshot().files_total,
        0,
        "local artifacts must not start a download batch"
    );
    assert!(
        !staging.join("libraries").exists(),
        "extraction is still owned by the official installer"
    );
    assert_eq!(std::fs::read(staging.join("installer.jar")).unwrap(), jar);
}

#[test]
fn forge_dependency_planning_covers_bundled_remote_and_generated_versions() {
    // Minimal metadata entries copied from the official installers, not version gates.
    let legacy = json!({
        "name": "net.minecraftforge:forge:1.12.2-14.23.5.2860",
        "downloads": {"artifact": {
            "path": "net/minecraftforge/forge/1.12.2-14.23.5.2860/forge-1.12.2-14.23.5.2860.jar",
            "url": "", "sha1": "029250575d3aa2cf80b56dffb66238a1eeaea2ac", "size": 4466148
        }}
    });
    let universal = json!({
        "name": "net.minecraftforge:forge:1.16.5-36.2.42:universal",
        "downloads": {"artifact": {
            "path": "net/minecraftforge/forge/1.16.5-36.2.42/forge-1.16.5-36.2.42-universal.jar",
            "url": "", "sha1": "fd07fe6c2c1cc7d3e403a1db7eadfa935ca7640d", "size": 2540080
        }}
    });
    let main = json!({
        "name": "net.minecraftforge:forge:1.16.5-36.2.42",
        "downloads": {"artifact": {
            "path": "net/minecraftforge/forge/1.16.5-36.2.42/forge-1.16.5-36.2.42.jar",
            "url": "", "sha1": "caca08d22543cbe101d64d171738e1ff6eef4de3", "size": 212563
        }}
    });
    let remote_forge = json!({
        "name": "net.minecraftforge:forge:1.20.1-47.2.0:universal",
        "downloads": {"artifact": {
            "path": "net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-universal.jar",
            "url": "https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-universal.jar",
            "sha1": "ac078f159add92ef9dce7a8259508c9d78470d3a", "size": 2672123
        }}
    });
    let generated = json!({
        "name": "net.minecraftforge:forge:1.21.4-54.1.22:client",
        "downloads": {"artifact": {
            "path": "net/minecraftforge/forge/1.21.4-54.1.22/forge-1.21.4-54.1.22-client.jar",
            "url": "", "sha1": "7e7d911be6deea351a1bbf77dda80fbc3e756eb1", "size": 29566778
        }}
    });
    let remote = json!({
        "name": "example:runtime:1",
        "downloads": {"artifact": {
            "path": "example/runtime/1/runtime-1.jar",
            "url": "https://libraries.minecraft.net/example/runtime/1/runtime-1.jar",
            "sha1": "0123456789abcdef0123456789abcdef01234567", "size": 42
        }}
    });
    for (version, profile_libs, version_libs, expected_remote_forge) in [
        (
            "1.12.2 / 14.23.5.2860",
            vec![legacy.clone()],
            vec![legacy],
            false,
        ),
        ("1.16.5 / 36.2.42", vec![universal], vec![main], false),
        ("1.20.1 / 47.2.0", vec![remote_forge], vec![], true),
        ("1.21.4 / 54.1.22", vec![], vec![generated], false),
    ] {
        let mut profile = json!({"libraries": profile_libs});
        let mut version_json = json!({"libraries": version_libs});
        // A shared real download must survive and be deduplicated across both documents.
        profile["libraries"]
            .as_array_mut()
            .unwrap()
            .push(remote.clone());
        version_json["libraries"]
            .as_array_mut()
            .unwrap()
            .push(remote.clone());
        let items =
            installer_library_items(&profile, &version_json, Path::new("staging/libraries"));
        assert_eq!(
            items.len(),
            if expected_remote_forge { 2 } else { 1 },
            "{version}: bundled/generated libraries must not be queued: {:?}",
            items.iter().map(|item| &item.label).collect::<Vec<_>>()
        );
        let runtime = items
            .iter()
            .find(|item| item.label == "安装器依赖 example:runtime:1")
            .unwrap();
        assert_eq!(
            runtime.dest,
            Path::new("staging/libraries/example/runtime/1/runtime-1.jar")
        );
        assert_eq!(
            runtime.official_url.as_deref(),
            Some("https://libraries.minecraft.net/example/runtime/1/runtime-1.jar")
        );
        assert_eq!(
            runtime.mirror_url.as_deref(),
            Some("https://bmclapi2.bangbang93.com/maven/example/runtime/1/runtime-1.jar")
        );
        assert_eq!(
            runtime.sha1.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(runtime.size, 42);
        assert!(runtime.prefer_mirror);
        assert_eq!(runtime.phase, "installer");
        if expected_remote_forge {
            let forge = items
                .iter()
                .find(|item| item.label.ends_with("1.20.1-47.2.0:universal"))
                .unwrap();
            assert_eq!(forge.official_url.as_deref(), Some("https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-universal.jar"));
            assert_eq!(
                forge.sha1.as_deref(),
                Some("ac078f159add92ef9dce7a8259508c9d78470d3a")
            );
            assert_eq!(forge.size, 2672123);
        }
    }
}

#[test]
fn installer_explicit_blank_urls_are_local_artifacts_for_any_coordinate() {
    for url in ["", " ", "\t\r\n"] {
        let lib = json!({
            "name": "example:bundled:1",
            "url": "https://repo.example/maven/",
            "downloads": {"artifact": {
                "path": "example/bundled/1/bundled-1.jar", "url": url
            }}
        });
        assert!(
            installer_library_item(&lib, Path::new("libraries")).is_none(),
            "explicit local marker {url:?} must override repository/mirror fallbacks"
        );
    }
}

#[test]
fn installer_absent_urls_preserve_repository_and_coordinate_fallbacks() {
    for (lib, official) in [
        (json!({"name": "example:runtime:1"}), None),
        (
            json!({"name": "example:runtime:1", "url": "https://repo.example/maven/"}),
            Some("https://repo.example/maven/example/runtime/1/runtime-1.jar"),
        ),
        (
            json!({"name": "example:runtime:1", "url": "https://repo.example/maven/", "downloads": {"artifact": {"url": null}}}),
            Some("https://repo.example/maven/example/runtime/1/runtime-1.jar"),
        ),
        (
            json!({"name": "example:runtime:1", "downloads": {"artifact": {"url": null}}}),
            None,
        ),
        (
            json!({"name": "example:runtime:1", "downloads": {"artifact": {"path": "example/runtime/1/runtime-1.jar"}}}),
            None,
        ),
    ] {
        let item = installer_library_item(&lib, Path::new("libraries"))
            .expect("missing URL is not an explicit local marker");
        assert_eq!(item.official_url.as_deref(), official);
        assert_eq!(
            item.mirror_url.as_deref(),
            Some("https://bmclapi2.bangbang93.com/maven/example/runtime/1/runtime-1.jar")
        );
        assert_eq!(
            item.dest,
            Path::new("libraries/example/runtime/1/runtime-1.jar")
        );
    }
}
