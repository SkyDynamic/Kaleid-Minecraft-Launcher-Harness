use super::*;
use kaleid_kernel::{
    apply_patches, parse_patch, DiagnosticRecord, KernelError, PluginSpec, Severity,
};
use serde_json::{json, Value};
use std::fs::{self, File, FileTimes};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

fn record(event: &str) -> DiagnosticRecord {
    DiagnosticRecord::new("launcher", event, Severity::Info).with_field("outcome", "succeeded")
}

fn persisted(logger: &LauncherLogger) -> Vec<Value> {
    let mut records = Vec::new();
    for suffix in [".4", ".3", ".2", ".1", ""] {
        let path = logger.run_dir().join(format!("launcher.log{suffix}"));
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                assert!(line.len() < 8192, "JSON plus newline exceeds policy");
                records.push(serde_json::from_str(line).expect("valid persisted JSON"));
            }
        }
    }
    records
}

fn emit(kernel: &Kernel, event: &str) {
    kernel.context().diagnostic(record(event));
}

// Catches accidental raw serialization or a missing output-boundary field/type policy.
#[test]
fn persisted_jsonl_keeps_unicode_correlations_but_not_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new();
    let logger = bootstrap_logger(&kernel, dir.path());
    let before = epoch_ms();
    let diagnostic = record("operation.finished")
        .with_operation_id("op-1")
        .with_session_id("session-2")
        .with_job_id("job-3")
        .with_batch_id("batch-4")
        .with_field("version_id", "版本😀\n\u{1}你好")
        .with_field("duration_ms", 42)
        .with_field("command", "launch_start")
        .with_field("source", "example.org:443")
        .with_field("password", "SENTINEL_PASSWORD")
        .with_field("access_token", "SENTINEL_TOKEN")
        .with_field("username", "SENTINEL_USER")
        .with_field("message", "SENTINEL_BODY")
        .with_field("args", json!(["SENTINEL_ARGS"]))
        .with_field("config", json!({"nested": "SENTINEL_CONFIG"}))
        .with_field("reason", "raw failure with SENTINEL_MESSAGE")
        .with_field("plugin_id", "C:\\SENTINEL_PATH\\secret")
        .with_field(
            "root_id",
            "https://user:SENTINEL_URL@host/path?token=secret",
        )
        .with_field("phase", json!({"value": "SENTINEL_NESTED"}));
    kernel.context().diagnostic(diagnostic);
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = persisted(&logger);
    let line = records
        .iter()
        .find(|r| r["event"] == "operation.finished")
        .unwrap();
    assert_eq!(line["severity"], "info");
    assert_eq!(line["operation_id"], "op-1");
    assert_eq!(line["session_id"], "session-2");
    assert_eq!(line["job_id"], "job-3");
    assert_eq!(line["batch_id"], "batch-4");
    assert_eq!(line["fields"]["version_id"], "版本😀\n\u{1}你好");
    assert_eq!(line["fields"]["duration_ms"], 42);
    assert_eq!(line["fields"]["command"], "launch_start");
    assert_eq!(line["fields"]["source"], "example.org:443");
    assert_eq!(line["run_id"], logger.run_id());
    assert!(line["timestamp_at_emission_ms"].as_u64().unwrap() >= before);
    assert!(line["sequence"].as_u64().unwrap() > 0);
    let text = fs::read_to_string(logger.run_dir().join("launcher.log")).unwrap();
    assert!(!text.contains("SENTINEL"));
    assert!(text.contains("\\n\\u0001"));
}

// Catches cutting a serialized JSON byte stream or forgetting worst-case escaping.
#[test]
fn record_limit_is_valid_json_even_for_large_control_and_unicode_fields() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new();
    let logger = bootstrap_logger(&kernel, dir.path());
    let mut diagnostic = record("bounded").with_operation_id("x".repeat(50000));
    for key in schema::ID_FIELDS {
        diagnostic = diagnostic.with_field(*key, "测\u{1}😀".repeat(20000));
    }
    for _ in 0..10 {
        kernel.context().diagnostic(diagnostic.clone());
    }
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = persisted(&logger);
    assert_eq!(
        records.iter().filter(|r| r["event"] == "bounded").count(),
        10
    );
    assert!(records.iter().all(|r| r["sequence"].is_u64()));
}

#[test]
fn diagnostic_host_strips_credentials_path_query_fragment_and_rejects_nonhosts() {
    for (input, expected) in [
        (
            "https://user:pass@EXAMPLE.org:8443/path?token=secret#body",
            Some("example.org:8443"),
        ),
        ("http://[::1]:8080/path", Some("[::1]:8080")),
        ("Example.org:443", Some("example.org:443")),
        ("https://example.org", Some("example.org")),
        ("C:\\Users\\secret", None),
        ("/home/user", None),
        ("file://secret/path", None),
        ("not a host", None),
        ("example.org/path", None),
        ("example.org?token=x", None),
        ("https://example.org:bad/path", None),
        ("https://", None),
        ("https://example.org\nsecret", None),
    ] {
        assert_eq!(diagnostic_host(input).as_deref(), expected, "{input:?}");
    }
}

#[test]
fn rotation_keeps_current_and_four_archives_without_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            rotation_bytes: 700,
            ..Options::default()
        },
    );
    for _ in 0..80 {
        logger.enqueue(&record("rotation"));
    }
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = persisted(&logger);
    assert!(!records.is_empty());
    for suffix in ["", ".1", ".2", ".3", ".4"] {
        assert!(
            fs::metadata(logger.run_dir().join(format!("launcher.log{suffix}")))
                .unwrap()
                .len()
                <= 700
        );
    }
    assert!(!logger.run_dir().join("launcher.log.5").exists());
    let seq: Vec<_> = records
        .iter()
        .map(|r| r["sequence"].as_u64().unwrap())
        .collect();
    assert!(seq.windows(2).all(|p| p[0] < p[1]));
    assert!(seq[0] > 1, "oldest records must actually have rotated away");
}

// Catches duplicate subscription/writer on both apply and repeated enable, independent kernels.
#[test]
fn bootstrap_is_idempotent_per_kernel_and_independent_for_concurrent_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new();
    crate::register_all(&kernel, dir.path());
    assert!(
        !dir.path().join("logs/launcher").exists(),
        "factory registration must do no I/O"
    );
    let logger = bootstrap_logger(&kernel, dir.path());
    let again = bootstrap_logger(&kernel, dir.path());
    assert!(Arc::ptr_eq(&logger, &again));
    let specs = vec![PluginSpec::new("launcher-logger").required()];
    kernel.load(&specs).unwrap();
    kernel.load(&specs).unwrap();
    kernel.enable("launcher-logger").unwrap();
    kernel.enable("launcher-logger").unwrap();
    emit(&kernel, "one.record");
    assert_eq!(flush_logger(&kernel), LoggerControlOutcome::Flushed);
    assert_eq!(
        persisted(&logger)
            .iter()
            .filter(|r| r["event"] == "one.record")
            .count(),
        1
    );
    assert_eq!(kernel.context().subscription_count(), 1);
    let other = Kernel::new();
    let other_log = bootstrap_logger(&other, dir.path());
    emit(&other, "other.record");
    assert_eq!(other_log.flush(), LoggerControlOutcome::Flushed);
    assert_ne!(logger.run_dir(), other_log.run_dir());
    assert!(!persisted(&logger)
        .iter()
        .any(|r| r["event"] == "other.record"));
    assert_eq!(
        persisted(&other_log)
            .iter()
            .filter(|r| r["event"] == "other.record")
            .count(),
        1
    );
}

#[test]
fn required_manifest_runtime_guards_and_minimal_disabled_version_manager_still_log() {
    let dir = tempfile::tempdir().unwrap();
    for mut specs in [crate::standard_preset(), crate::minimal_preset()] {
        let mut bad = PluginSpec::new("wrong")
            .with_id("launcher-logger")
            .disabled();
        bad.when.push(kaleid_kernel::Predicate::Platform {
            platform: "never".into(),
        });
        specs.insert(0, bad);
        ensure_logger_spec(&mut specs);
        assert_eq!(
            specs.iter().filter(|s| s.id == "launcher-logger").count(),
            1
        );
        assert_eq!(specs[0].name, "launcher-logger");
        assert!(specs[0].required && specs[0].enabled && specs[0].when.is_empty());
        for patch in [
            "- update:\n    id: launcher-logger\n    enabled: false",
            "- remove:\n    id: launcher-logger",
            "- update:\n    id: launcher-logger\n    when:\n      - platform: windows",
        ] {
            let ops = parse_patch(patch).unwrap();
            assert!(apply_patches(&specs, &ops).is_err());
        }
    }
    let kernel = Kernel::new();
    crate::register_all(&kernel, dir.path());
    let logger = bootstrap_logger(&kernel, dir.path());
    kernel.load(&crate::minimal_preset()).unwrap();
    assert!(
        !kernel
            .specs()
            .iter()
            .find(|s| s.id == "version-manager")
            .unwrap()
            .enabled
    );
    assert!(matches!(
        kernel.disable("launcher-logger"),
        Err(KernelError::Required(_))
    ));
    emit(&kernel, "still.usable");
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    assert!(persisted(&logger)
        .iter()
        .any(|r| r["event"] == "still.usable"));
    assert!(crate::describe_plugins(&kernel)
        .iter()
        .any(|p| p.id == "launcher-logger" && p.name == "启动器日志" && p.required));
}

#[test]
fn saturated_producer_drops_nonblocking_and_flush_persists_summary() {
    let dir = tempfile::tempdir().unwrap();
    let (release, gate) = crossbeam_channel::bounded(1);
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            queue_capacity: 2,
            start_gate: Some(gate),
            ..Options::default()
        },
    );
    for _ in 0..5 {
        logger.enqueue(&record("queued"));
    }
    release.send(()).unwrap();
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = persisted(&logger);
    assert_eq!(records.iter().filter(|r| r["event"] == "queued").count(), 2);
    let dropped: u64 = records
        .iter()
        .filter(|r| r["event"] == "records.dropped")
        .map(|r| r["fields"]["dropped_records"].as_u64().unwrap())
        .sum();
    assert_eq!(dropped, 3);
}

#[test]
fn full_queue_shutdown_ack_is_bounded_and_eventually_drains() {
    let dir = tempfile::tempdir().unwrap();
    let (release, gate) = crossbeam_channel::bounded(1);
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            queue_capacity: 1,
            ack_timeout: Duration::from_millis(25),
            start_gate: Some(gate),
            ..Options::default()
        },
    );
    logger.enqueue(&record("before.shutdown"));
    let start = Instant::now();
    assert_eq!(logger.shutdown(), LoggerControlOutcome::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    assert!(matches!(
        logger.done.recv_timeout(Duration::from_secs(1)),
        Err(crossbeam_channel::RecvTimeoutError::Disconnected)
    ));
    assert_eq!(logger.shutdown(), LoggerControlOutcome::Flushed);
    let records = persisted(&logger);
    assert!(records.iter().any(|r| r["event"] == "before.shutdown"));
    assert_eq!(
        records
            .iter()
            .filter(|r| r["event"] == "writer.shutdown")
            .count(),
        1
    );
    logger.enqueue(&record("after.shutdown"));
    assert!(!persisted(&logger)
        .iter()
        .any(|r| r["event"] == "after.shutdown"));
}

#[test]
fn cleanup_unsubscribes_drains_and_kernel_drop_works_with_retained_context() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new();
    let logger = bootstrap_logger(&kernel, dir.path());
    let ctx = kernel.context().clone();
    emit(&kernel, "before.drop");
    drop(kernel);
    assert_eq!(ctx.subscription_count(), 0);
    assert_eq!(logger.shutdown(), LoggerControlOutcome::Flushed);
    let records = persisted(&logger);
    assert!(records.iter().any(|r| r["event"] == "before.drop"));
    assert!(records
        .iter()
        .any(|r| r["event"] == "kernel.shutdown" && r["fields"]["outcome"] == "started"));
    assert_eq!(
        records
            .iter()
            .filter(|r| r["event"] == "writer.shutdown")
            .count(),
        1
    );
}

#[test]
fn dropping_last_sender_terminates_worker_without_explicit_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let logger = LauncherLogger::start(dir.path(), Options::default());
    let path = logger.run_dir().to_owned();
    let done = logger.done.clone();
    logger.enqueue(&record("last.sender"));
    drop(logger);
    assert!(matches!(
        done.recv_timeout(Duration::from_secs(1)),
        Err(crossbeam_channel::RecvTimeoutError::Disconnected)
    ));
    let text = fs::read_to_string(path.join("launcher.log")).unwrap();
    assert!(text.contains("last.sender") && text.contains("writer.shutdown"));
}

#[test]
fn unwritable_path_fails_open_and_reopens_only_after_thirty_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blocked");
    fs::write(&root, b"not a directory").unwrap();
    let clock = Arc::new(AtomicU64::new(1_800_000_000_000));
    let elapsed = Arc::new(AtomicU64::new(0));
    let logger = LauncherLogger::start(
        &root,
        Options {
            clock: {
                let c = clock.clone();
                Arc::new(move || c.load(Ordering::Relaxed))
            },
            monotonic_clock: {
                let c = elapsed.clone();
                Arc::new(move || Duration::from_millis(c.load(Ordering::Relaxed)))
            },
            ..Options::default()
        },
    );
    logger.enqueue(&record("lost"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    fs::remove_file(&root).unwrap(); // Verified test-owned path: dir/blocked.
    fs::create_dir(&root).unwrap();
    clock.fetch_add(29_000, Ordering::Relaxed);
    elapsed.fetch_add(29_000, Ordering::Relaxed);
    logger.enqueue(&record("too.early"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    assert!(!logger.run_dir().exists());
    clock.fetch_add(1_000, Ordering::Relaxed);
    elapsed.fetch_add(1_000, Ordering::Relaxed);
    logger.enqueue(&record("recovered.record"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let records = persisted(&logger);
    assert!(records.iter().any(|r| r["event"] == "writer.recovered"));
    assert!(records.iter().any(|r| r["event"] == "recovered.record"));
}

fn stale_run(base: &Path, name: &str, time: u64) -> PathBuf {
    let path = base.join(name);
    fs::create_dir_all(&path).unwrap();
    for name in ["launcher.log", ".active"] {
        let file = File::create(path.join(name)).unwrap();
        file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(time)))
            .unwrap();
    }
    path
}

#[test]
fn retention_deletes_only_stale_known_unlocked_sessions_and_uses_newest_file() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("logs/launcher");
    let now = 1_800_000_000_000;
    let old = now - 8 * 86400 * 1000;
    let dead = stale_run(
        &base,
        "1700000000000-123-12345678-1234-4234-8234-123456789abc",
        old,
    );
    let active = stale_run(
        &base,
        "1700000000000-124-12345678-1234-4234-8234-123456789abc",
        old,
    );
    let marker = File::options()
        .read(true)
        .write(true)
        .open(active.join(".active"))
        .unwrap();
    marker.try_lock().unwrap();
    let unknown = stale_run(
        &base,
        "1700000000000-125-12345678-1234-4234-8234-123456789abc",
        old,
    );
    fs::write(unknown.join("unrecognized.bin"), b"preserve").unwrap();
    let fresh = stale_run(
        &base,
        "1700000000000-126-12345678-1234-4234-8234-123456789abc",
        old,
    );
    let latest = File::create(fresh.join("launcher.log.1")).unwrap();
    latest
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(now)))
        .unwrap();
    let foreign = stale_run(&base, "not-a-logger-run", old);
    let console = dir.path().join("logs/game-console");
    fs::create_dir_all(&console).unwrap();
    fs::write(console.join("game.log"), b"keep").unwrap();
    retention::cleanup_old_runs(&base, now);
    assert!(!dead.exists());
    for path in [&active, &unknown, &fresh, &foreign, &console] {
        assert!(path.exists(), "{path:?}");
    }
    drop(marker);
    retention::cleanup_old_runs(&base, now);
    assert!(!active.exists());
}

#[test]
fn file_open_failure_does_not_make_plugin_apply_fail() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("file");
    fs::write(&blocked, b"blocked").unwrap();
    let kernel = Kernel::new();
    crate::register_all(&kernel, &blocked);
    assert!(kernel
        .load(&[PluginSpec::new("launcher-logger").required()])
        .is_ok());
    assert_eq!(flush_logger(&kernel), LoggerControlOutcome::Degraded);
    let start = Instant::now();
    assert_eq!(shutdown_logger(&kernel), LoggerControlOutcome::Degraded);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn sink_rejects_url_host_fields_and_pathlike_metadata_instead_of_serializing_them() {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::new();
    let logger = bootstrap_logger(&kernel, dir.path());
    let mut rec = DiagnosticRecord::new(
        "C:\\SECRET_COMPONENT",
        "https://secret/event",
        Severity::Warn,
    );
    rec = rec
        .with_operation_id("/secret/op")
        .with_field("source", "https://secret.example/path")
        .with_field("host", "user:SECRET@host")
        .with_field("command", "secret argument text")
        .with_field("outcome", "succeeded")
        .with_field("total_bytes", json!({"SECRET": 1}))
        .with_field("unknown_count", 123);
    kernel.context().diagnostic(rec);
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    let text = fs::read_to_string(logger.run_dir().join("launcher.log")).unwrap();
    assert!(!text.contains("secret") && !text.contains("SECRET"));
    let records = persisted(&logger);
    assert_eq!(records[0]["component"], "unknown");
    assert_eq!(records[0]["event"], "unknown");
    assert_eq!(records[0]["fields"], json!({"outcome": "succeeded"}));
}

#[test]
fn worker_write_failure_is_nonpanicking_and_flush_shutdown_do_not_busy_loop() {
    let dir = tempfile::tempdir().unwrap();
    let fault = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let logger = LauncherLogger::start(
        dir.path(),
        Options {
            write_fault: Some(fault.clone()),
            ..Options::default()
        },
    );
    logger.enqueue(&record("healthy"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Flushed);
    fault.store(true, Ordering::Relaxed);
    logger.enqueue(&record("faulted"));
    assert_eq!(logger.flush(), LoggerControlOutcome::Degraded);
    assert_eq!(logger.shutdown(), LoggerControlOutcome::Degraded);
}
