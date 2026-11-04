#[test]
fn diagnostics_fix1_failure_before_reentrant_cancel_stays_failed_for_batch_and_owner() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits = requests.clone();
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut head = [0; 4096];
                let _ = stream.read(&mut head);
                hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let (_dir, mut service, req, _dest) = diagnostic_mod_fixture(&url, b"expected bytes");
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
    let downloads = Arc::new(
        service
            .downloads
            .as_ref()
            .unwrap()
            .as_ref()
            .clone()
            .with_context(kernel.context().clone()),
    );
    service.downloads = Some(downloads.clone());
    let cancel_from_failed_status = downloads.clone();
    kernel
        .context()
        .on::<kaleid_kernel::DiagnosticRecord>(move |r| {
            if r.event == "download.status" && r.fields.get("status") == Some(&json!("failed")) {
                if let Some(job) = r
                    .job_id
                    .as_ref()
                    .and_then(|id| cancel_from_failed_status.job(id))
                {
                    job.cancel();
                }
            }
        });
    let service = service.with_context(kernel.context().clone());
    let queued = service.install(&req).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let captured = records.lock().unwrap().clone();
        if let Some(owner) = captured.iter().find(|r| r.event == "operation.finished") {
            let job = downloads.job(&queued.job_id).unwrap();
            // Legacy scheduler precedence and wrapper state/error are deliberately unchanged.
            assert!(job.cancelled());
            assert!(job.snapshot().error.unwrap().contains("任务已取消"));
            let batch = captured
                .iter()
                .find(|r| r.event == "download.batch.finished")
                .unwrap();
            for record in [owner, batch] {
                assert_eq!(
                    record.fields["outcome"], "failed",
                    "fatal cause was masked by cancellation"
                );
                assert_eq!(record.fields["cancelled"], true);
                assert_eq!(record.level, kaleid_kernel::Severity::Error);
            }
            assert_eq!(
                captured
                    .iter()
                    .filter(|r| r.event == "operation.finished")
                    .count(),
                1
            );
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    server.join().unwrap();
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}
