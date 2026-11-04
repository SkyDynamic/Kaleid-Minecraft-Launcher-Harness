use super::*;
use kaleid_kernel::Severity;
use serde_json::{json, Value};
use std::fs;

fn lines(logger: &LauncherLogger) -> Vec<Value> {
    fs::read_to_string(logger.run_dir().join("launcher.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn rec(event: &str) -> DiagnosticRecord {
    DiagnosticRecord::new("launcher", event, Severity::Info)
}

// A rejected encoded record is one loss, not an append loss plus a caller loss.
#[test]
fn one_oversized_record_counts_as_exactly_one_drop() {
    let dir = tempfile::tempdir().unwrap();
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            rotation_bytes: 400,
            ..Options::default()
        },
    );
    logger.enqueue(&rec("oversized").with_field("version_id", "版本".repeat(128)));
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = lines(&logger);
    assert!(!records.iter().any(|r| r["event"] == "oversized"));
    let count: u64 = records
        .iter()
        .filter(|r| r["event"] == "records.dropped")
        .map(|r| r["fields"]["dropped_records"].as_u64().unwrap())
        .sum();
    assert_eq!(count, 1);
}

#[test]
fn coordinated_producer_schema_fields_are_persisted_as_flat_safe_scalars() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new();
    let logger = bootstrap_logger(&kernel, dir.path());
    let diagnostic = rec("download.batch.finished")
        .with_field("operation", "download_batch")
        .with_field("previous_status", "queued")
        .with_field("source", "mojang")
        .with_field("previous_source", "bmclapi")
        .with_field("policy", "retry_budget")
        .with_field("retry_class", "connection")
        .with_field("host", "example.org")
        .with_field("previous_host", "mirror.example.org:443")
        .with_field("runtime_duration_ms", 10)
        .with_field("attempt", 2)
        .with_field("throttle_count", 3)
        .with_field("source_fallback_count", 4)
        .with_field("range_fallback_count", 5)
        .with_field("integrity_failure_count", 6)
        .with_field("internal_error_count", 7)
        .with_field("warnings_suppressed", 8)
        .with_field("enabled_preserved", true);
    kernel.context().diagnostic(diagnostic);
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    assert_eq!(
        lines(&logger)[0]["fields"],
        json!({
            "operation":"download_batch", "previous_status":"queued", "source":"mojang",
            "previous_source":"bmclapi", "policy":"retry_budget", "retry_class":"connection",
            "host":"example.org", "previous_host":"mirror.example.org:443", "runtime_duration_ms":10,
            "attempt":2, "throttle_count":3, "source_fallback_count":4, "range_fallback_count":5,
            "integrity_failure_count":6, "internal_error_count":7, "warnings_suppressed":8,
            "enabled_preserved":true,
        })
    );
}

#[test]
fn simultaneous_bootstrap_calls_share_one_sink_and_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new();
    let handles = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..12)
            .map(|_| scope.spawn(|| bootstrap_logger(&kernel, dir.path())))
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    for handle in &handles {
        assert!(Arc::ptr_eq(&handles[0], handle));
    }
    kernel.context().diagnostic(rec("one.concurrent.record"));
    assert_eq!(handles[0].flush(), LoggerControlOutcome::Flushed);
    assert_eq!(kernel.context().subscription_count(), 1);
    assert_eq!(
        lines(&handles[0])
            .iter()
            .filter(|r| r["event"] == "one.concurrent.record")
            .count(),
        1
    );
}

#[test]
fn shutdown_of_full_queue_persists_drops_without_a_prior_flush() {
    let dir = tempfile::tempdir().unwrap();
    let (release, gate) = bounded(1);
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            queue_capacity: 1,
            start_gate: Some(gate),
            ..Options::default()
        },
    );
    for _ in 0..4 {
        logger.enqueue(&rec("queued"));
    }
    release.send(()).unwrap();
    assert_eq!(logger.shutdown(), LoggerControlOutcome::Flushed);
    let records = lines(&logger);
    assert_eq!(records.iter().filter(|r| r["event"] == "queued").count(), 1);
    assert_eq!(
        records
            .iter()
            .filter(|r| r["event"] == "writer.shutdown")
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r["event"] == "records.dropped")
            .map(|r| r["fields"]["dropped_records"].as_u64().unwrap())
            .sum::<u64>(),
        3
    );
}

#[test]
fn worker_panic_isolated_from_caller_and_completion_not_left_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            clock: {
                let c = calls.clone();
                Arc::new(move || {
                    if c.fetch_add(1, Ordering::Relaxed) == 0 {
                        1_800_000_000_000
                    } else {
                        panic!("injected worker-only clock panic")
                    }
                })
            },
            ..Options::default()
        },
    );
    assert!(matches!(
        logger.done.recv_timeout(Duration::from_secs(1)),
        Err(crossbeam_channel::RecvTimeoutError::Disconnected)
    ));
    logger.enqueue(&rec("after.worker.exit"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    assert_eq!(logger.shutdown(), LoggerControlOutcome::Degraded);
}

#[test]
fn reopen_repairs_an_incomplete_last_line_before_appending_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(1_800_000_000_000));
    let elapsed = Arc::new(AtomicU64::new(0));
    let fault = Arc::new(AtomicBool::new(false));
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            clock: {
                let c = clock.clone();
                Arc::new(move || c.load(Ordering::Relaxed))
            },
            monotonic_clock: {
                let c = elapsed.clone();
                Arc::new(move || Duration::from_millis(c.load(Ordering::Relaxed)))
            },
            write_fault: Some(fault.clone()),
            ..Options::default()
        },
    );
    logger.enqueue(&rec("before.partial"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    // Emulate the exact persisted state of a write_all that wrote a partial JSON line.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(logger.run_dir().join("launcher.log"))
        .unwrap();
    file.write_all(b"{\"partial\":").unwrap();
    drop(file);
    fault.store(true, Ordering::Relaxed);
    logger.enqueue(&rec("lost.during.failure"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    fault.store(false, Ordering::Relaxed);
    clock.fetch_add(30_000, Ordering::Relaxed);
    elapsed.fetch_add(30_000, Ordering::Relaxed);
    logger.enqueue(&rec("after.partial"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = lines(&logger);
    assert!(records.iter().any(|r| r["event"] == "before.partial"));
    assert!(records.iter().any(|r| r["event"] == "after.partial"));
    assert_eq!(
        records
            .iter()
            .filter(|r| r["event"] == "writer.recovered")
            .count(),
        1
    );
}

#[test]
fn wall_clock_forward_jump_does_not_bypass_reopen_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blocked-clock");
    fs::write(&root, b"not a directory").unwrap();
    let epoch = Arc::new(AtomicU64::new(1_800_000_000_000));
    let elapsed = Arc::new(AtomicU64::new(0));
    let logger = LauncherLogger::start(
        &root,
        Options {
            clock: {
                let c = epoch.clone();
                Arc::new(move || c.load(Ordering::Relaxed))
            },
            monotonic_clock: {
                let c = elapsed.clone();
                Arc::new(move || Duration::from_millis(c.load(Ordering::Relaxed)))
            },
            ..Options::default()
        },
    );
    logger.enqueue(&rec("before.clock.jump"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    // Verified test-owned dir/blocked-clock; make I/O usable without advancing elapsed time.
    fs::remove_file(&root).unwrap();
    fs::create_dir(&root).unwrap();
    epoch.fetch_add(86_400_000, Ordering::Relaxed);
    logger.enqueue(&rec("wall.jump.only"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    assert!(
        !logger.run_dir().exists(),
        "epoch correction must not retry I/O early"
    );
}

#[test]
fn monotonic_elapsed_reopens_despite_backward_wall_jump() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blocked-backward-clock");
    fs::write(&root, b"not a directory").unwrap();
    let epoch = Arc::new(AtomicU64::new(1_800_000_000_000));
    let elapsed = Arc::new(AtomicU64::new(0));
    let logger = LauncherLogger::start(
        &root,
        Options {
            clock: {
                let c = epoch.clone();
                Arc::new(move || c.load(Ordering::Relaxed))
            },
            monotonic_clock: {
                let c = elapsed.clone();
                Arc::new(move || Duration::from_millis(c.load(Ordering::Relaxed)))
            },
            ..Options::default()
        },
    );
    logger.enqueue(&rec("before.backward.jump"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    // Verified test-owned dir/blocked-backward-clock; replace only this temporary fixture.
    fs::remove_file(&root).unwrap();
    fs::create_dir(&root).unwrap();
    epoch.store(1_700_000_000_000, Ordering::Relaxed);
    elapsed.store(29_999, Ordering::Relaxed);
    logger.enqueue(&rec("elapsed.too.early"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    elapsed.store(30_000, Ordering::Relaxed);
    logger.enqueue(&rec("elapsed.recovered"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = lines(&logger);
    assert!(records.iter().any(|r| r["event"] == "elapsed.recovered"));
    let recovery = records
        .iter()
        .find(|r| r["event"] == "writer.recovered")
        .unwrap();
    assert_eq!(recovery["timestamp_at_emission_ms"], 1_700_000_000_000u64);
}

#[test]
fn retention_skips_preexisting_symlink_or_junction_to_external_logs() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("logs/launcher");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&base).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("launcher.log"), b"keep outside").unwrap();
    fs::write(outside.join(".active"), b"").unwrap();
    let link = base.join("1700000000000-123-12345678-1234-4234-8234-123456789abc");
    #[cfg(windows)]
    {
        let result = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(outside.to_string_lossy().replace('/', "\\"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "junction fixture failed: {:?}",
            result
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    retention::cleanup_old_runs(&base, u64::MAX);
    assert!(link.exists());
    assert_eq!(
        fs::read(outside.join("launcher.log")).unwrap(),
        b"keep outside"
    );
}
