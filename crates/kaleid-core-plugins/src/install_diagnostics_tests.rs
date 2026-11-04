#[cfg(windows)]
#[test]
fn diagnostics_installer_real_child_exit_timeout_and_pending_cancel_are_honest() {
    let (dir, _root, service) = service_with_root();
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
    for (index, script, timeout, outcome, cancel) in [
        (
            0,
            "echo SECRET_CHILD_BODY & exit /b 7",
            Duration::from_secs(5),
            "failed",
            false,
        ),
        (1, "exit /b 0", Duration::from_secs(5), "succeeded", true),
        (
            2,
            "for /l %x in (1,1,1000000000) do @rem",
            Duration::ZERO,
            "failed",
            false,
        ),
    ] {
        let job = Job::new(
            format!("installer-{index}"),
            "fixture".into(),
            "fixture".into(),
            "r".into(),
            "1.20.1".into(),
            LoaderKind::Forge,
            None,
            "official-first".into(),
        );
        if cancel {
            job.cancel();
        }
        let log = dir.path().join(format!("installer-{index}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let mut command = std::process::Command::new("cmd");
        command
            .args(["/c", script])
            .stdout(std::process::Stdio::from(out.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(out));
        let mut operation = Operation::new(service.ctx.clone(), "install", "installer");
        operation.record = operation.record.with_job_id(&job.id);
        let result =
            service.run_installer_command(command, &job, "Fixture", &log, timeout, &mut operation);
        assert_eq!(result.is_ok(), outcome == "succeeded");
        let captured = records.lock().unwrap().clone();
        let ended: Vec<_> = captured
            .iter()
            .filter(|r| {
                r.event == "installer.finished" && r.job_id.as_deref() == Some(job.id.as_str())
            })
            .collect();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].fields["outcome"], outcome);
        assert_eq!(ended[0].fields["cancelled"], cancel);
        assert_eq!(ended[0].fields["loader"], "forge");
        assert!(ended[0].fields.contains_key("pid"));
        if index == 0 {
            assert_eq!(ended[0].fields["exit_code"], 7);
        }
        if index == 2 {
            assert_eq!(ended[0].fields["category"], "timeout");
        }
        assert!(!serde_json::to_string(&captured)
            .unwrap()
            .contains("SECRET_CHILD_BODY"));
    }
}

#[test]
fn diagnostics_install_worker_reports_failure_after_acceptance_without_raw_paths() {
    let (dir, mut root, service) = service_with_root();
    let kernel = kaleid_kernel::Kernel::new();
    let records = Arc::new(Mutex::new(Vec::<kaleid_kernel::DiagnosticRecord>::new()));
    let captured = records.clone();
    kernel
        .context()
        .on::<kaleid_kernel::DiagnosticRecord>(move |r| captured.lock().unwrap().push(r.clone()));
    let service = service.with_context(kernel.context().clone());
    let obstruction = dir.path().join("SECRET_ROOT");
    std::fs::write(&obstruction, "file").unwrap();
    root.path = obstruction.to_string_lossy().to_string();
    let req = InstallRequest {
        root_id: root.id.clone(),
        game_version: "1.20.1".into(),
        version_name: "fixture".into(),
        loader: InstallLoader::Vanilla,
        install_fabric_api: false,
        optifine: None,
    };
    let queued = service.enqueue(root, req).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let done: Vec<_> = records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.event == "operation.finished")
            .cloned()
            .collect();
        if !done.is_empty() {
            assert_eq!(done.len(), 1);
            assert_eq!(done[0].job_id.as_deref(), Some(queued.job_id.as_str()));
            assert_eq!(done[0].fields["operation"], "install");
            assert_eq!(done[0].fields["outcome"], "failed");
            assert_eq!(done[0].fields["root_id"], "r");
            assert!(!serde_json::to_string(&done)
                .unwrap()
                .contains("SECRET_ROOT"));
            break;
        }
        assert!(Instant::now() < deadline, "worker result never diagnosed");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn diagnostics_install_preflight_and_thread_failure_finish_once() {
    let (_dir, root, service) = service_with_root();
    let kernel = kaleid_kernel::Kernel::new();
    let records = Arc::new(Mutex::new(Vec::<kaleid_kernel::DiagnosticRecord>::new()));
    let captured = records.clone();
    kernel
        .context()
        .on::<kaleid_kernel::DiagnosticRecord>(move |r| captured.lock().unwrap().push(r.clone()));
    let service = service.with_context(kernel.context().clone());
    let req = InstallRequest {
        root_id: root.id.clone(),
        game_version: "1.20.1".into(),
        version_name: "../SECRET".into(),
        loader: InstallLoader::Vanilla,
        install_fabric_api: false,
        optifine: None,
    };
    assert!(service.enqueue(root.clone(), req.clone()).is_err());
    crate::downloads::diagnostics::fail_next_spawn();
    let mut valid = req;
    valid.version_name = "fixture".into();
    assert!(service.enqueue(root, valid).is_err());
    let records = records.lock().unwrap();
    let ended: Vec<_> = records
        .iter()
        .filter(|r| r.event == "operation.finished")
        .collect();
    assert_eq!(ended.len(), 2);
    assert!(ended.iter().all(|r| r.fields["outcome"] == "failed"));
    assert!(ended.iter().any(|r| r.fields["category"] == "thread_spawn"));
    assert!(!serde_json::to_string(&*records).unwrap().contains("SECRET"));
}
