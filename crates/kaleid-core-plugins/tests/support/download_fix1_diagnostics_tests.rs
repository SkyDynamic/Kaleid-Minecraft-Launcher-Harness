#[test]
fn diagnostics_fix1_probe_integrity_recovery_is_observed_but_not_terminal_failure() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_records(kernel.context());
    let manager = manager(dir.path()).with_context(kernel.context().clone());
    let body = body_of(1024);
    let server = TestServer::start(body.clone(), Mode::CorruptFirstBody);
    let dest = dir.path().join("probe.jar");
    manager
        .run_batch(
            job("probe-recovered"),
            vec![unknown_size_item(
                "probe",
                format!("{}/file", server.base),
                &dest,
                &body,
            )],
        )
        .unwrap();
    assert_eq!(std::fs::read(dest).unwrap(), body);
    assert_eq!(
        server.requests.load(Ordering::SeqCst),
        2,
        "preserve one probe plus one ordinary attempt"
    );
    let records = records.lock().unwrap();
    let integrity = records
        .iter()
        .find(|r| r.event == "download.integrity_failure")
        .unwrap();
    assert_eq!(integrity.fields["reason"], "probe");
    assert_eq!(integrity.fields["category"], "digest_mismatch");
    let ended = records
        .iter()
        .find(|r| r.event == "download.batch.finished")
        .unwrap();
    assert_eq!(ended.fields["outcome"], "succeeded");
    assert_eq!(ended.fields["failed"], 0);
    assert_eq!(ended.fields["integrity_failure_count"], 1);
}

#[test]
fn diagnostics_fix1_shared_job_failure_does_not_contaminate_healthy_batch_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_records(kernel.context());
    let manager = manager(dir.path()).with_context(kernel.context().clone());
    let shared = job("shared-fatal");
    manager.register(shared.clone());
    let watched = manager.clone();
    kernel
        .context()
        .on::<kaleid_kernel::DiagnosticRecord>(move |r| {
            if r.event == "download.status"
                && r.fields.get("status") == Some(&serde_json::json!("failed"))
            {
                watched.cancel("shared-fatal").unwrap();
            }
        });
    let failed = manager.begin_session(shared.clone());
    let healthy = manager.begin_session(shared.clone());
    let body = body_of(1024);
    let server = TestServer::start(body.clone(), Mode::Status404);
    failed
        .push(item(
            "fatal",
            format!("{}/file", server.base),
            &dir.path().join("file.jar"),
            &body,
        ))
        .unwrap();
    let business_error = failed.close_and_wait().unwrap_err().to_string();
    assert!(
        business_error.contains("任务已取消"),
        "preserve existing API cancellation precedence"
    );
    assert!(healthy
        .close_and_wait()
        .unwrap_err()
        .to_string()
        .contains("任务已取消"));
    let records = records.lock().unwrap();
    let failed = records
        .iter()
        .find(|r| r.event == "download.batch.finished" && r.fields["files_total"] == 1)
        .unwrap();
    let healthy = records
        .iter()
        .find(|r| r.event == "download.batch.finished" && r.fields["files_total"] == 0)
        .unwrap();
    assert_eq!(failed.fields["outcome"], "failed");
    assert_eq!(failed.level, kaleid_kernel::Severity::Error);
    assert_eq!(failed.fields["failed"], 1);
    assert_eq!(healthy.fields["outcome"], "cancelled");
    assert_eq!(healthy.level, kaleid_kernel::Severity::Info);
    assert_eq!(healthy.fields["failed"], 0);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
}

#[test]
fn diagnostics_fix1_probe_429_and_terminal_404_preserve_typed_fields_and_requests() {
    for (unknown, mode, status, class) in [
        (
            true,
            Mode::Throttle {
                times: 1,
                retry_after: Some(4),
            },
            429,
            "throttle",
        ),
        (false, Mode::Status404, 404, "http"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let kernel = kaleid_kernel::Kernel::new();
        let records = diagnostic_records(kernel.context());
        let manager = manager(dir.path()).with_context(kernel.context().clone());
        let body = body_of(1024);
        let server = TestServer::start(body.clone(), mode);
        let mut download = item(
            "SECRET_LABEL",
            format!("{}/file?token=SECRET_QUERY", server.base),
            &dir.path().join("file.jar"),
            &body,
        );
        if unknown {
            download.size = 0;
        }
        assert!(manager
            .run_batch(job("http-diagnostics"), vec![download])
            .is_err());
        assert_eq!(
            server.requests.load(Ordering::SeqCst),
            1,
            "diagnostics must add no retries or requests"
        );
        let records = records.lock().unwrap();
        let observed = records
            .iter()
            .find(|r| r.fields.get("http_status") == Some(&serde_json::json!(status)))
            .expect("typed HTTP status was discarded");
        assert_eq!(observed.fields["retry_class"], class);
        assert_eq!(observed.fields["category"], class);
        assert!(observed.fields["host"]
            .as_str()
            .unwrap()
            .starts_with("127.0.0.1:"));
        if unknown {
            assert_eq!(observed.fields["retry_after_ms"], 4000);
        }
        assert!(
            !records.iter().any(|r| r.event == "download.retry"),
            "no retry actually happened"
        );
        assert!(records.len() < 24);
        assert!(!serde_json::to_string(&*records).unwrap().contains("SECRET"));
    }
}

#[test]
fn diagnostics_fix1_no_address_counts_one_terminal_file_failure() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_records(kernel.context());
    let manager = manager(dir.path()).with_context(kernel.context().clone());
    let mut download = item(
        "missing",
        "http://unused.example/".into(),
        &dir.path().join("missing"),
        &[1],
    );
    download.official_url = None;
    assert!(manager
        .run_batch(job("no-address"), vec![download])
        .is_err());
    let records = records.lock().unwrap();
    let ended = records
        .iter()
        .find(|r| r.event == "download.batch.finished")
        .unwrap();
    assert_eq!(ended.fields["failed"], 1);
    assert_eq!(
        records
            .iter()
            .filter(|r| r.event == "download.failed")
            .count(),
        1
    );
}

#[test]
fn diagnostics_fix1_retry_attempt_cancel_is_not_a_terminal_failure() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = diagnostic_records(kernel.context());
    let manager = manager(dir.path()).with_context(kernel.context().clone());
    let shared = job("retry-cancel");
    manager.register(shared.clone());
    let watched = shared.clone();
    kernel
        .context()
        .on::<kaleid_kernel::DiagnosticRecord>(move |r| {
            if r.event == "download.retry" {
                watched.cancel();
            }
        });
    let body = body_of(1024);
    let server = TestServer::start(body.clone(), Mode::Status500First(1));
    assert!(manager
        .run_batch(
            shared,
            vec![item(
                "retry",
                format!("{}/file", server.base),
                &dir.path().join("file.jar"),
                &body
            )]
        )
        .unwrap_err()
        .to_string()
        .contains("任务已取消"));
    let records = records.lock().unwrap();
    let ended = records
        .iter()
        .find(|r| r.event == "download.batch.finished")
        .unwrap();
    assert_eq!(ended.fields["outcome"], "cancelled");
    assert_eq!(ended.fields["failed"], 0);
    assert_eq!(ended.level, kaleid_kernel::Severity::Info);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
}
