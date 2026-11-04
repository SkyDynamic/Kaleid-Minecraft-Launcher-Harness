//! Dedicated worker: the only owner of file handles, rotation, sequence, and failure episodes.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use kaleid_kernel::{DiagnosticRecord, Severity};

use super::{fixed_warning, retention, schema, LoggerControlOutcome, Message, Options, Shared};

pub(super) struct Completion {
    pub shared: Arc<Shared>,
    pub completed: Sender<()>,
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        // Dropping the zero-capacity completion sender broadcasts disconnect to all waiters.
        let _ = &self.completed;
    }
}

struct Output {
    run_dir: PathBuf,
    run_id: String,
    file: Option<File>,
    marker: Option<File>,
    bytes: u64,
    sequence: u64,
    next_attempt: Duration,
    episode: bool,
    shared: Arc<Shared>,
    options: Options,
}

impl Output {
    fn now(&self) -> u64 {
        (self.options.clock)()
    }
    fn failure(&mut self) {
        self.file = None;
        self.next_attempt = (self.options.monotonic_clock)().saturating_add(super::REOPEN_INTERVAL);
        self.shared.degraded.store(true, Ordering::Release);
        if !self.episode {
            fixed_warning();
        }
        self.episode = true;
    }

    fn open(&mut self) -> bool {
        if self.file.is_some() {
            return true;
        }
        if (self.options.monotonic_clock)() < self.next_attempt {
            return false;
        }
        if self.open_inner().is_err() {
            self.failure();
            return false;
        }
        let recovering = self.episode;
        // Opening is not recovery until a real write and flush succeed.
        if recovering {
            let record = self.internal("writer.recovered", "succeeded", Severity::Info);
            if !self.append(&record) || !self.flush_file() {
                return false;
            }
        }
        self.episode = false;
        self.shared.degraded.store(false, Ordering::Release);
        true
    }

    fn open_inner(&mut self) -> io::Result<()> {
        fs::create_dir_all(&self.run_dir)?;
        // Refuse a replaced/symlinked run dir or marker/log instead of following it.
        if fs::symlink_metadata(&self.run_dir)?
            .file_type()
            .is_symlink()
        {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if self.marker.is_none() {
            let path = self.run_dir.join(retention::MARKER);
            if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
            {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            let marker = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)?;
            marker
                .try_lock()
                .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
            self.marker = Some(marker);
        }
        let path = self.run_dir.join("launcher.log");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        // FILE_APPEND_DATA alone cannot truncate a partial line on Windows. The marker
        // gives exclusive ownership, so use read/write and explicitly seek to the end.
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        self.bytes = Self::repair_tail(&mut file)?;
        file.seek(SeekFrom::End(0))?;
        self.file = Some(file);
        Ok(())
    }

    fn repair_tail(file: &mut File) -> io::Result<u64> {
        let original = file.metadata()?.len();
        let mut end = original;
        let mut buffer = [0u8; super::MAX_RECORD_BYTES];
        while end > 0 {
            let start = end.saturating_sub(buffer.len() as u64);
            let size = (end - start) as usize;
            file.seek(SeekFrom::Start(start))?;
            file.read_exact(&mut buffer[..size])?;
            if let Some(last_newline) = buffer[..size].iter().rposition(|b| *b == b'\n') {
                let complete = start + last_newline as u64 + 1;
                if complete != original {
                    file.set_len(complete)?;
                }
                return Ok(complete);
            }
            end = start;
        }
        if original != 0 {
            file.set_len(0)?;
        }
        Ok(0)
    }

    fn internal(&self, event: &str, outcome: &str, severity: Severity) -> schema::SafeRecord {
        let mut record = DiagnosticRecord::new("launcher-logger", event, severity)
            .with_field("outcome", outcome);
        record.timestamp_at_emission_ms = self.now();
        schema::sanitize(&record)
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }
        let archive = |index| self.run_dir.join(format!("launcher.log.{index}"));
        match fs::remove_file(archive(super::ARCHIVE_COUNT)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        for index in (1..super::ARCHIVE_COUNT).rev() {
            match fs::rename(archive(index), archive(index + 1)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        fs::rename(self.run_dir.join("launcher.log"), archive(1))?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.run_dir.join("launcher.log"))?;
        file.seek(SeekFrom::End(0))?;
        self.file = Some(file);
        self.bytes = 0;
        Ok(())
    }

    fn append(&mut self, record: &schema::SafeRecord) -> bool {
        self.sequence = self.sequence.saturating_add(1);
        // Loss ownership belongs to the caller: producer record=one, summary=restore count.
        let Some(bytes) = schema::encode(record, &self.run_id, self.sequence) else {
            return false;
        };
        if bytes.len() as u64 > self.options.rotation_bytes {
            return false;
        }
        if self.bytes.saturating_add(bytes.len() as u64) > self.options.rotation_bytes
            && self.bytes > 0
        {
            if self.rotate().is_err() {
                self.failure();
                return false;
            }
        }
        #[cfg(test)]
        if self
            .options
            .write_fault
            .as_ref()
            .is_some_and(|fault| fault.load(Ordering::Relaxed))
        {
            self.failure();
            return false;
        }
        let Some(file) = self.file.as_mut() else {
            return false;
        };
        let start = self.bytes;
        match file.write_all(&bytes) {
            Ok(()) => {
                self.bytes = self.bytes.saturating_add(bytes.len() as u64);
                true
            }
            Err(_) => {
                // Best-effort rollback of a partial line; never intentionally persist truncated JSON.
                let _ = file.set_len(start);
                self.failure();
                false
            }
        }
    }

    fn write(&mut self, record: &schema::SafeRecord) {
        if !self.open() || !self.append(record) {
            self.shared.drop_records(1);
        }
    }

    fn dropped_summary(&mut self) {
        if !self.open() {
            return;
        }
        let count = self.shared.dropped.swap(0, Ordering::AcqRel);
        if count == 0 {
            return;
        }
        let mut record =
            DiagnosticRecord::new("launcher-logger", "records.dropped", Severity::Warn)
                .with_field("dropped_records", count)
                .with_field("outcome", "dropped");
        record.timestamp_at_emission_ms = self.now();
        if !self.append(&schema::sanitize(&record)) {
            self.shared.drop_records(count);
        }
    }

    fn flush_file(&mut self) -> bool {
        let Some(file) = self.file.as_mut() else {
            return false;
        };
        if file.flush().is_err() {
            self.failure();
            return false;
        }
        true
    }

    fn flush(&mut self) -> LoggerControlOutcome {
        if self.shared.observer_failed.swap(false, Ordering::AcqRel) {
            // Notification is handled on worker, never stderr I/O on a business producer.
            if !self.episode {
                fixed_warning();
            }
            self.episode = true;
        }
        self.dropped_summary();
        if self.open() && self.flush_file() {
            self.shared.outcome()
        } else {
            LoggerControlOutcome::Degraded
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Record(record) => {
                self.dropped_summary();
                self.write(&record);
            }
            Message::Flush(ack) => {
                let outcome = self.flush();
                let _ = ack.try_send(outcome);
            }
            Message::Wake => {}
        }
    }
}

pub(super) fn run(
    receiver: Receiver<Message>,
    shared: Arc<Shared>,
    run_dir: PathBuf,
    run_id: String,
    options: Options,
) {
    #[cfg(test)]
    if let Some(gate) = &options.start_gate {
        let _ = gate.recv();
    }
    if let Some(base) = run_dir.parent() {
        retention::cleanup_old_runs(base, (options.clock)());
    }
    let mut output = Output {
        run_dir,
        run_id,
        file: None,
        marker: None,
        bytes: 0,
        sequence: 0,
        next_attempt: Duration::ZERO,
        episode: false,
        shared: shared.clone(),
        options,
    };
    let mut disconnected = false;
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(message) => {
                output.handle(message);
                // Bound each batch even if producers continuously refill the queue.
                let batch_start = Instant::now();
                for _ in 1..64 {
                    if batch_start.elapsed() >= Duration::from_secs(1) {
                        break;
                    }
                    match receiver.try_recv() {
                        Ok(message) => output.handle(message),
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                        Err(TryRecvError::Empty) => break,
                    }
                }
                let _ = output.flush();
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => disconnected = true,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
        if (disconnected || shared.closing.load(Ordering::SeqCst))
            && shared.in_flight.load(Ordering::SeqCst) == 0
            && receiver.is_empty()
        {
            break;
        }
    }
    output.dropped_summary();
    let final_record = output.internal("writer.shutdown", "succeeded", Severity::Info);
    output.write(&final_record);
    let _ = output.flush();
    // Handles (including marker lock) drop before Completion broadcasts termination.
}
