#[test]
fn diagnostics_fix1_actual_success_after_ignored_failed_batch_stays_successful() {
    use std::io::{Read, Write};
    let dir = tempfile::tempdir().unwrap();
    let kernel = kaleid_kernel::Kernel::new();
    let records = Arc::new(Mutex::new(Vec::<DiagnosticRecord>::new()));
    let captured = records.clone();
    kernel.context().on::<DiagnosticRecord>(move |r| {
        if let Ok(mut records) = captured.lock() {
            records.push(r.clone());
        }
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        for response in [
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx".as_slice(),
        ] {
            loop {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut head = [0; 4096];
                    let _ = stream.read(&mut head);
                    stream.write_all(response).unwrap();
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    });
    let settings = Arc::new(Settings::open(dir.path()));
    let manager = DownloadManager::new(settings).with_context(kernel.context().clone());
    let job = plan_job("official-only");
    let mut download = plan_item(1);
    download.official_url = Some(url);
    download.mirror_url = None;
    download.dest = dir.path().join("file.jar");
    let operation =
        diagnostics::Operation::new(Some(kernel.context().clone()), "install", "install");
    operation.accepted();
    assert!(manager
        .run_batch(job.clone(), vec![download.clone()])
        .is_err());
    assert!(job.diagnostic_failed());
    let recovered = manager.run_batch(job.clone(), vec![download.clone()]);
    assert!(recovered.is_ok());
    job.cancel(); // A pending request after actual success must not rewrite the actual outcome.
    operation.finish_with_failure(&recovered, job.cancelled(), job.diagnostic_failed());
    server.join().unwrap();
    assert_eq!(std::fs::read(download.dest).unwrap(), b"x");
    let records = records.lock().unwrap();
    let owner = records
        .iter()
        .find(|r| r.event == "operation.finished")
        .unwrap();
    assert_eq!(owner.fields["outcome"], "succeeded");
    assert_eq!(owner.fields["cancelled"], true);
    assert_eq!(owner.level, Severity::Info);
}

#[test]
fn diagnostics_fix1_mixed_range_error_success_drain_counts_failure_once_without_reordering() {
    let kernel = kaleid_kernel::Kernel::new();
    let records = Arc::new(Mutex::new(Vec::<DiagnosticRecord>::new()));
    let captured = records.clone();
    kernel.context().on::<DiagnosticRecord>(move |r| {
        if let Ok(mut records) = captured.lock() {
            records.push(r.clone());
        }
    });
    let job = plan_job("official-first");
    let batch = BatchDiagnostics::new(Some(kernel.context().clone()), &job.id);
    let mut file = segmented_file(1, 4, "https://node.example/file", Some(true));
    file.diagnostics = Some(batch.clone());
    file.inflight = 2;
    let mut files = vec![file];
    let limits = HostLimits::for_concurrency(4);
    let mut first_error = None;
    on_report(
        &job,
        &mut files,
        WorkReport::RangeErr(1, (0, 1), "terminal range failure".into()),
        &mut first_error,
        &limits,
    );
    assert!(files[0].failed.is_some());
    assert!(
        first_error.is_none(),
        "do not move legacy first_error installation earlier"
    );
    on_report(
        &job,
        &mut files,
        WorkReport::RangeOk(1, 2, 3),
        &mut first_error,
        &limits,
    );
    assert!(files[0].finished);
    assert!(
        first_error.is_none(),
        "leave legacy final scheduler fallback in charge"
    );
    batch.finish(
        &Err("terminal range failure".into()),
        false,
        0,
        1,
        Some(0),
        Some(4),
    );
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
