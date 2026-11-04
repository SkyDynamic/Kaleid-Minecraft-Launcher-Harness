//! Conservative retention, limited to recognized, inactive launcher run directories.
//!
//! Contract: launcher-owned trusted data and cooperating launcher instances only.
//! External actors must not concurrently rename a run directory or any ancestor, or
//! replace those entries with symlinks/junctions during cleanup. Marker locks and path
//! checks protect cooperating active runs and reject pre-existing links; they are not
//! a race-resistant security boundary against arbitrary same-user filesystem replacement.
use std::fs::{self, File, Metadata};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(super) const MARKER: &str = ".active";

fn regular(metadata: &Metadata) -> bool {
    metadata.is_file() && !metadata.file_type().is_symlink()
}

fn run_name(name: &str) -> bool {
    let mut parts = name.splitn(3, '-');
    let Some(stamp) = parts.next() else {
        return false;
    };
    let Some(pid) = parts.next() else {
        return false;
    };
    let Some(id) = parts.next() else { return false };
    !stamp.is_empty()
        && stamp.len() <= 20
        && stamp.bytes().all(|b| b.is_ascii_digit())
        && pid.parse::<u32>().ok().filter(|p| *p > 0).is_some()
        && uuid::Uuid::parse_str(id)
            .ok()
            .is_some_and(|parsed| parsed.hyphenated().to_string() == id)
}

fn recognized_file(name: &str) -> bool {
    matches!(
        name,
        MARKER
            | "launcher.log"
            | "launcher.log.1"
            | "launcher.log.2"
            | "launcher.log.3"
            | "launcher.log.4"
    )
}

fn snapshot(path: &Path) -> Option<(Vec<PathBuf>, u64)> {
    let mut files = Vec::new();
    let mut newest = 0;
    let mut has_marker = false;
    let mut has_log = false;
    for entry in fs::read_dir(path).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        if !recognized_file(name) {
            return None;
        }
        let meta = fs::symlink_metadata(entry.path()).ok()?;
        if !regular(&meta) {
            return None;
        }
        let time = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis();
        newest = newest.max(time.min(u64::MAX as u128) as u64);
        has_marker |= name == MARKER;
        has_log |= name.starts_with("launcher.log");
        files.push(entry.path());
    }
    (has_marker && has_log).then_some((files, newest))
}

pub(super) fn cleanup_old_runs(base: &Path, now: u64) {
    // Reject pre-existing symlinked logs/launcher directories, including the logs parent.
    for path in [Some(base), base.parent()].into_iter().flatten() {
        let Ok(meta) = fs::symlink_metadata(path) else {
            return;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return;
        }
    }
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(run_name) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        let Some((_, newest)) = snapshot(&path) else {
            continue;
        };
        if now.saturating_sub(newest) <= super::RETENTION_DAYS * 86400 * 1000 {
            continue;
        }
        let Ok(marker) = File::options()
            .read(true)
            .write(true)
            .open(path.join(MARKER))
        else {
            continue;
        };
        if marker.try_lock().is_err() {
            continue;
        }
        // Recheck after locking. Unknown/new/active files make this a no-op.
        let Some((files, newest)) = snapshot(&path) else {
            continue;
        };
        if now.saturating_sub(newest) <= super::RETENTION_DAYS * 86400 * 1000 {
            continue;
        }
        let mut removed = true;
        // All targets are from this validated directory's recognized regular-file snapshot.
        for file in files
            .iter()
            .filter(|file| file.file_name().is_some_and(|n| n != MARKER))
        {
            if fs::remove_file(file).is_err() {
                removed = false;
                break;
            }
        }
        if removed && fs::remove_file(path.join(MARKER)).is_ok() {
            let _ = fs::remove_dir(&path); // No recursive deletion, even if an entry races in.
        }
        drop(marker);
    }
}
