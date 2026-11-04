fn diagnostic_capture(
    ctx: &kaleid_kernel::Context,
) -> Arc<Mutex<Vec<kaleid_kernel::DiagnosticRecord>>> {
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = records.clone();
    ctx.on::<kaleid_kernel::DiagnosticRecord>(move |r| {
        if let Ok(mut records) = captured.lock() {
            records.push(r.clone());
        }
    });
    records
}

#[test]
fn diagnostics_repair_panic_does_not_inherit_legacy_ok_result() {
    assert_eq!(repair_outcome(&Ok(()), true), "failed");
    assert_eq!(repair_outcome(&Ok(()), false), "succeeded");
    assert_eq!(
        repair_outcome(&Err(ApiError::invalid("任务已取消")), false),
        "cancelled"
    );
}

#[cfg(windows)]
#[test]
fn diagnostics_launch_repairs_report_joined_results_without_false_root_identity() {
    use kaleid_api::SettingsServiceApi;
    for succeeds in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mc = dir.path().join("mc");
        std::fs::create_dir_all(mc.join("versions/fixture")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/client?token=SECRET_QUERY",
            listener.local_addr().unwrap()
        );
        let body = b"SECRET_BODY";
        let hash = if succeeds {
            use sha1::Digest;
            hex::encode(sha1::Sha1::digest(body))
        } else {
            "0".repeat(40)
        };
        std::fs::write(mc.join("versions/fixture/fixture.json"), json!({"id":"fixture","downloads":{"client":{"url":url,"size":body.len(),"sha1":hash}},"libraries":[]}).to_string()).unwrap();
        let server = std::thread::spawn(move || {
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
        });
        let kernel = kaleid_kernel::Kernel::new();
        let records = diagnostic_capture(kernel.context());
        let settings = Arc::new(crate::Settings::open(dir.path()));
        settings
            .patch_global(&json!({"downloadSource":"official-only"}))
            .unwrap();
        let downloads =
            Arc::new(DownloadManager::new(settings).with_context(kernel.context().clone()));
        let manager = GameProcessManager::new(dir.path())
            .with_downloads(Some(downloads))
            .with_context(kernel.context().clone());
        let mut p = plan();
        p.version_id = "fixture".into();
        p.game_version = "1.20.1".into();
        p.java_path = "cmd".into();
        p.mc_dir = mc.to_string_lossy().into();
        p.game_dir = p.mc_dir.clone();
        p.launch_dir = p.mc_dir.clone();
        p.libraries_dir = mc.join("libraries").to_string_lossy().into();
        p.libraries_dirs = vec![p.libraries_dir.clone()];
        p.natives_dir = mc.join("natives").to_string_lossy().into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.main_class.clear();
        p.game_args = vec!["/c".into(), "exit /b 0".into()];
        manager.start(p, None, Vec::new()).unwrap();
        wait_for(
            &manager,
            if succeeds {
                LaunchPhase::Exited
            } else {
                LaunchPhase::Failed
            },
        );
        server.join().unwrap();
        let captured = records.lock().unwrap().clone();
        let repairs: Vec<_> = captured
            .iter()
            .filter(|r| r.event == "launch.repair_finished")
            .collect();
        assert_eq!(repairs.len(), 1);
        assert_eq!(
            repairs[0].fields["outcome"],
            if succeeds { "succeeded" } else { "failed" }
        );
        assert_eq!(repairs[0].job_id.as_deref(), Some("repair-1-0"));
        assert_eq!(repairs[0].fields["files_total"], 1);
        assert!(!captured.iter().any(|r| r.fields.contains_key("root_id")));
        assert!(!serde_json::to_string(&captured).unwrap().contains("SECRET"));
    }
}

#[test]
fn diagnostics_launch_preparation_failure_is_terminal_and_excludes_launch_payload() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_capture(kernel.context());
    let manager = GameProcessManager::new(dir.path()).with_context(kernel.context().clone());
    let mut p = plan();
    p.java_path = dir
        .path()
        .join("SECRET_JAVA_PATH")
        .to_string_lossy()
        .to_string();
    p.username = "SECRET_USERNAME".into();
    p.extra_jvm_args.push("-Dtoken=SECRET_JVM".into());
    manager
        .start(
            p,
            Some("SECRET_ACCESS_TOKEN".into()),
            vec!["SECRET_MOD".into()],
        )
        .unwrap();
    wait_for(&manager, LaunchPhase::Failed);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let captured = records.lock().unwrap().clone();
        let ended: Vec<_> = captured
            .iter()
            .filter(|r| r.event == "operation.finished")
            .collect();
        if !ended.is_empty() {
            assert_eq!(ended.len(), 1);
            assert_eq!(ended[0].fields["operation"], "launch");
            assert_eq!(ended[0].fields["outcome"], "failed");
            assert_eq!(ended[0].session_id.as_deref(), Some("1"));
            assert!(
                !ended[0].fields.contains_key("root_id"),
                "LaunchState root_id is not genuine root context"
            );
            let text = serde_json::to_string(&captured).unwrap();
            assert!(!text.contains("SECRET"), "launch data leaked: {text}");
            break;
        }
        assert!(Instant::now() < deadline, "no terminal launch record");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn diagnostics_launch_thread_creation_failure_finishes_once() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_capture(kernel.context());
    let manager = GameProcessManager::new(dir.path()).with_context(kernel.context().clone());
    crate::downloads::diagnostics::fail_next_spawn();
    assert!(manager.start(plan(), None, Vec::new()).is_err());
    let captured = records.lock().unwrap().clone();
    let ended: Vec<_> = captured
        .iter()
        .filter(|r| r.event == "operation.finished")
        .collect();
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].fields["category"], "thread_spawn");
    // Diagnostic-only change must retain the old state even on thread creation failure.
    assert_eq!(manager.state().phase, LaunchPhase::Preparing);
}

#[cfg(windows)]
#[test]
fn diagnostics_launch_cancel_is_observed_and_steps_do_not_repeat() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_capture(kernel.context());
    let manager = GameProcessManager::new(dir.path()).with_context(kernel.context().clone());
    manager.set_prep_delay(100);
    let started = manager.start(plan(), None, Vec::new()).unwrap();
    manager.cancel(started.session_id).unwrap();
    wait_for(&manager, LaunchPhase::Cancelled);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let captured = records.lock().unwrap().clone();
        if let Some(ended) = captured.iter().find(|r| r.event == "operation.finished") {
            assert_eq!(ended.fields["outcome"], "cancelled");
            assert!(captured
                .iter()
                .any(|r| r.event == "launch.cancel_requested"));
            assert!(!captured.iter().any(|r| r.event == "launch.spawned"));
            assert_eq!(captured.iter().filter(|r| r.event == "launch.step" && r.fields.get("step") == Some(&json!("check")) && r.fields.get("status") == Some(&json!("skipped"))).count(), 1);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
#[test]
fn diagnostics_launch_background_running_and_exit_distinguish_user_stop() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_capture(kernel.context());
    let manager = GameProcessManager::new(dir.path()).with_context(kernel.context().clone());
    for _ in 0..2 {
        let mut p = plan();
        p.java_path = "cmd".into();
        p.classpath.clear();
        p.jvm_args.clear();
        p.extra_jvm_args.clear();
        p.main_class.clear();
        p.game_args = vec![
            "/c".into(),
            "echo Backend library: LWJGL & ping -n 10 127.0.0.1".into(),
        ];
        manager.start(p, None, Vec::new()).unwrap();
    }
    wait_for(&manager, LaunchPhase::Running);
    assert!(manager.cancel(1).is_err());
    manager.stop(None).unwrap();
    wait_for(&manager, LaunchPhase::Exited);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let captured = records.lock().unwrap().clone();
        if captured
            .iter()
            .filter(|r| r.event == "operation.finished")
            .count()
            == 2
        {
            for id in ["1", "2"] {
                let selected: Vec<_> = captured
                    .iter()
                    .filter(|r| r.session_id.as_deref() == Some(id))
                    .collect();
                assert_eq!(
                    selected
                        .iter()
                        .filter(|r| r.event == "launch.spawned")
                        .count(),
                    1
                );
                assert_eq!(
                    selected
                        .iter()
                        .filter(|r| r.event == "launch.running")
                        .count(),
                    1
                );
                for step in ["spawn", "window"] {
                    assert_eq!(
                        selected
                            .iter()
                            .filter(|r| r.event == "launch.step"
                                && r.fields["step"] == step
                                && r.fields["status"] == "completed")
                            .count(),
                        1,
                        "missing actual completed step {step}"
                    );
                }
                let running = selected
                    .iter()
                    .find(|r| r.event == "launch.running")
                    .unwrap();
                assert_eq!(running.fields["reason"], "log_marker");
                let exited = selected
                    .iter()
                    .find(|r| r.event == "launch.exited")
                    .unwrap();
                assert_eq!(exited.fields["killed"], true);
                assert_eq!(exited.fields["crashed"], false);
                let mut steps = std::collections::HashSet::new();
                for record in selected.iter().filter(|r| r.event == "launch.step") {
                    assert!(
                        steps.insert((
                            record.fields["step"].to_string(),
                            record.fields["status"].to_string()
                        )),
                        "duplicate step record"
                    );
                }
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing asynchronous exit results"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
