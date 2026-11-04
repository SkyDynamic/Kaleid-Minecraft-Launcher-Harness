//! Always-enabled launcher diagnostics. Business callbacks only sanitize and try_send.
//! File failures are fail-open; explicit flush/shutdown waits at most one second.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{bounded, Receiver, Sender};
use kaleid_kernel::{Context, DiagnosticRecord, Kernel, Plugin, PluginResult, PluginSpec};
use serde_json::Value;

#[cfg(test)]
mod reliability_tests;
mod retention;
mod schema;
#[cfg(test)]
mod tests;
mod writer;

pub use schema::diagnostic_host;
pub const LOGGER_ID: &str = "launcher-logger";
pub const QUEUE_CAPACITY: usize = 2048;
pub const MAX_RECORD_BYTES: usize = 8 * 1024;
pub const ROTATION_BYTES: u64 = 5 * 1024 * 1024;
pub const ARCHIVE_COUNT: usize = 4;
pub const RETENTION_DAYS: u64 = 7;
pub const REOPEN_INTERVAL: Duration = Duration::from_secs(30);
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);

/// Acknowledged output state, not a business-operation result. Closed means no sink exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerControlOutcome {
    Flushed,
    Degraded,
    TimedOut,
    Closed,
}

struct Shared {
    closing: AtomicBool,
    stopped: AtomicBool,
    degraded: AtomicBool,
    observer_failed: AtomicBool,
    in_flight: AtomicUsize,
    dropped: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Self {
            closing: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            degraded: AtomicBool::new(false),
            observer_failed: AtomicBool::new(false),
            in_flight: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
        }
    }
    fn drop_records(&self, n: u64) {
        let _ = self
            .dropped
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                Some(old.saturating_add(n))
            });
    }
    fn outcome(&self) -> LoggerControlOutcome {
        if self.degraded.load(Ordering::Acquire) {
            LoggerControlOutcome::Degraded
        } else {
            LoggerControlOutcome::Flushed
        }
    }
}

enum Message {
    Record(Box<schema::SafeRecord>),
    Flush(Sender<LoggerControlOutcome>),
    Wake,
}

struct Options {
    queue_capacity: usize,
    rotation_bytes: u64,
    ack_timeout: Duration,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    monotonic_clock: Arc<dyn Fn() -> Duration + Send + Sync>,
    #[cfg(test)]
    start_gate: Option<Receiver<()>>,
    #[cfg(test)]
    write_fault: Option<Arc<AtomicBool>>,
}

impl Default for Options {
    fn default() -> Self {
        let origin = Instant::now();
        Self {
            queue_capacity: QUEUE_CAPACITY,
            rotation_bytes: ROTATION_BYTES,
            ack_timeout: CONTROL_TIMEOUT,
            clock: Arc::new(epoch_ms),
            monotonic_clock: Arc::new(move || origin.elapsed()),
            #[cfg(test)]
            start_gate: None,
            #[cfg(test)]
            write_fault: None,
        }
    }
}

/// A sink handle. It deliberately contains no Context, avoiding registry/subscriber cycles.
pub struct LauncherLogger {
    run_id: String,
    run_dir: PathBuf,
    sender: Sender<Message>,
    shared: Arc<Shared>,
    done: Receiver<()>,
    ack_timeout: Duration,
}

impl LauncherLogger {
    fn start(data_root: &Path, options: Options) -> Self {
        let run_id = format!(
            "{}-{}-{}",
            (options.clock)(),
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let run_dir = data_root.join("logs").join("launcher").join(&run_id);
        let (sender, receiver) = bounded(options.queue_capacity.max(1));
        let (completed, done) = bounded(0);
        let shared = Arc::new(Shared::new());
        let logger = Self {
            run_id: run_id.clone(),
            run_dir: run_dir.clone(),
            sender,
            shared: shared.clone(),
            done,
            ack_timeout: options.ack_timeout.min(CONTROL_TIMEOUT),
        };
        let worker_shared = shared.clone();
        let result = std::thread::Builder::new()
            .name("kaleid-launcher-log".into())
            .spawn(move || {
                // No Sender<Message>, LauncherLogger, or Context is moved into this worker.
                let completion = writer::Completion {
                    shared: worker_shared.clone(),
                    completed,
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    writer::run(receiver, worker_shared.clone(), run_dir, run_id, options);
                }));
                if result.is_err() {
                    worker_shared.degraded.store(true, Ordering::Release);
                    fixed_warning();
                }
                drop(completion);
            });
        // Never join: an OS filesystem call may be stuck, but caller waits remain bounded.
        if result.is_err() {
            shared.degraded.store(true, Ordering::Release);
            shared.stopped.store(true, Ordering::Release);
            fixed_warning();
        }
        logger
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }
    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    fn enqueue(&self, record: &DiagnosticRecord) {
        struct InFlight<'a>(&'a AtomicUsize);
        impl Drop for InFlight<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        // Admission and closing use one global atomic order: an admitted in-flight
        // producer must be visible before the worker decides that draining is complete.
        self.shared.in_flight.fetch_add(1, Ordering::SeqCst);
        let _guard = InFlight(&self.shared.in_flight);
        if self.shared.closing.load(Ordering::SeqCst) || self.shared.stopped.load(Ordering::Acquire)
        {
            return;
        }
        let safe = schema::sanitize(record);
        match self.sender.try_send(Message::Record(Box::new(safe))) {
            Ok(()) => {}
            Err(crossbeam_channel::TrySendError::Full(_)) => self.shared.drop_records(1),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                self.shared.degraded.store(true, Ordering::Release);
            }
        }
    }

    /// FIFO barrier for all preceding successful enqueue calls; one total bounded deadline.
    pub fn flush(&self) -> LoggerControlOutcome {
        if self.shared.stopped.load(Ordering::Acquire) {
            return self.shared.outcome();
        }
        if self.shared.closing.load(Ordering::Acquire) {
            return self.wait_done();
        }
        let deadline = Instant::now() + self.ack_timeout;
        let (ack, result) = bounded(1);
        match self
            .sender
            .send_timeout(Message::Flush(ack), self.ack_timeout)
        {
            Ok(()) => match result.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(outcome) => outcome,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => LoggerControlOutcome::TimedOut,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => self.shared.outcome(),
            },
            Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => LoggerControlOutcome::TimedOut,
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => self.shared.outcome(),
        }
    }

    /// Stop accepting, drain accepted records, write the direct final marker, then flush.
    /// A timeout never causes an unbounded join. Repeated calls observe the same completion.
    pub fn shutdown(&self) -> LoggerControlOutcome {
        self.shared.closing.store(true, Ordering::SeqCst);
        let _ = self.sender.try_send(Message::Wake);
        self.wait_done()
    }

    fn wait_done(&self) -> LoggerControlOutcome {
        if self.shared.stopped.load(Ordering::Acquire) {
            return self.shared.outcome();
        }
        match self.done.recv_timeout(self.ack_timeout) {
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => LoggerControlOutcome::TimedOut,
            _ => self.shared.outcome(),
        }
    }
}

fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn fixed_warning() {
    // Never format an error or record. No recursive diagnostic publication.
    let _ = std::io::stderr().write_all(b"Kaleid launcher logging is temporarily unavailable.\n");
}

// Only bootstrap/apply takes this lock, never diagnostic/business callbacks.
static BOOTSTRAP: Mutex<()> = Mutex::new(());

fn ensure_context(ctx: &Context, data_root: &Path) -> Arc<LauncherLogger> {
    let _guard = match BOOTSTRAP.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(logger) = ctx.service::<LauncherLogger>() {
        return logger;
    }
    ctx.with_owner(LOGGER_ID, |ctx| {
        ctx.provide(
            LOGGER_ID,
            LauncherLogger::start(data_root, Options::default()),
            0,
        );
        // provide creates the service. Kernel's registry locking is outside this sink's callback.
        if let Some(logger) = ctx.service::<LauncherLogger>() {
            let cleanup = logger.clone();
            ctx.on_dispose(move || {
                let _ = cleanup.shutdown();
            });
            let observer = logger.clone();
            ctx.on::<DiagnosticRecord>(move |record| {
                // Best-effort for unwind profiles only; expected paths never panic.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    observer.enqueue(record)
                }))
                .is_err()
                {
                    observer
                        .shared
                        .observer_failed
                        .store(true, Ordering::Release);
                    observer.shared.drop_records(1);
                }
            });
            return logger;
        }
        // This branch is only reachable if another registry owner removed the just-added service.
        // Return a detached fail-open sink rather than turning a file failure into Apply failure.
        Arc::new(LauncherLogger::start(data_root, Options::default()))
    })
}

/// Call immediately after factory registration, BEFORE manifests or Kernel::load.
pub fn bootstrap_logger(kernel: &Kernel, data_root: &Path) -> Arc<LauncherLogger> {
    ensure_context(kernel.context(), data_root)
}

/// Trusted host boundary: restore one unconditional, enabled, canonical first logger spec.
pub fn ensure_logger_spec(specs: &mut Vec<PluginSpec>) {
    specs.retain(|spec| spec.id != LOGGER_ID && spec.name != LOGGER_ID);
    specs.insert(0, PluginSpec::new(LOGGER_ID).required());
}

pub fn flush_logger(kernel: &Kernel) -> LoggerControlOutcome {
    kernel
        .context()
        .service::<LauncherLogger>()
        .map(|sink| sink.flush())
        .unwrap_or(LoggerControlOutcome::Closed)
}
pub fn shutdown_logger(kernel: &Kernel) -> LoggerControlOutcome {
    kernel
        .context()
        .service::<LauncherLogger>()
        .map(|sink| sink.shutdown())
        .unwrap_or(LoggerControlOutcome::Closed)
}

/// Builtin plugin shell; no optional services or disabling configuration.
pub struct LauncherLoggerPlugin {
    data_root: PathBuf,
}
impl LauncherLoggerPlugin {
    pub fn new(data_root: &Path) -> Self {
        Self {
            data_root: data_root.to_owned(),
        }
    }
}
impl Plugin for LauncherLoggerPlugin {
    fn id(&self) -> &str {
        LOGGER_ID
    }
    fn apply(&self, ctx: &Context, _cfg: Value) -> PluginResult {
        let _ = ensure_context(ctx, &self.data_root);
        Ok(())
    }
}

#[cfg(test)]
mod preset_regressions {
    #[test]
    fn logger_is_first_required_in_both_presets() {
        for specs in [crate::standard_preset(), crate::minimal_preset()] {
            assert_eq!(specs[0].id, "launcher-logger");
            assert!(specs[0].required && specs[0].enabled && specs[0].when.is_empty());
        }
    }
}
