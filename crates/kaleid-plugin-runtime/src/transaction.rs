//! Same-volume recoverable filesystem transactions, with bounded capture write-ahead custody.
use crate::{
    io_error, manifest,
    package::{self, ScanMode, TreeIndex},
    policy, PackageFile, PackageManager,
};
use kaleid_api::{validate_relative_entry, PluginError};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalOperation {
    Install,
    Update,
    Remove,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    Capturing,
    Prepared,
    OldMoved,
    Published,
    RemoveCommitted,
    Restoring,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    File,
    Directory,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureIntent {
    pub relative_path: String,
    pub kind: CaptureKind,
    pub max_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionOwner {
    pub schema_version: u32,
    pub transaction: String,
}
/// Paths are derived from plugin_id and a generated basename, never journal absolute paths.
/// Backup and candidate raw indexes include the receipt; payload digest excludes installer files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransactionJournal {
    pub schema_version: u32,
    pub transaction: String,
    pub plugin_id: String,
    pub operation: JournalOperation,
    pub phase: JournalPhase,
    pub candidate_digest: Option<String>,
    pub backup_files: Vec<PackageFile>,
    pub backup_directories: Vec<String>,
    pub candidate_files: Vec<PackageFile>,
    pub candidate_directories: Vec<String>,
    pub capture_intents: Vec<CaptureIntent>,
}
pub(crate) struct Transaction {
    pub path: PathBuf,
    pub journal: TransactionJournal,
}
fn bad() -> PluginError {
    manifest::record_error()
}
fn valid_basename(name: &str) -> bool {
    name.strip_prefix(policy::TXN_PREFIX).is_some_and(|s| {
        s.len() == 32
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn valid_dirs(dirs: &[String]) -> bool {
    let mut previous: Option<&str> = None;
    dirs.iter().all(|p| {
        let okay = validate_relative_entry(p).is_ok() && previous.is_none_or(|s| s < p.as_str());
        previous = Some(p);
        okay
    })
}
fn validate(j: &TransactionJournal, name: &str) -> Result<(), PluginError> {
    policy::validate_id(&j.plugin_id).map_err(|_| bad())?;
    if j.schema_version != 1
        || j.transaction != name
        || !valid_basename(name)
        || !package::valid_tree_index(&j.backup_files, true)
        || !package::valid_tree_index(&j.candidate_files, true)
        || !valid_dirs(&j.backup_directories)
        || !valid_dirs(&j.candidate_directories)
    {
        return Err(bad());
    }
    if let Some(d) = &j.candidate_digest {
        if d.len() != 64
            || !d
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(bad());
        }
    }
    if j.phase == JournalPhase::Capturing
        && (j.candidate_digest.is_some()
            || !j.candidate_files.is_empty()
            || !j.candidate_directories.is_empty())
    {
        return Err(bad());
    }
    let mut paths = std::collections::HashMap::new();
    for intent in &j.capture_intents {
        if validate_relative_entry(&intent.relative_path).is_err()
            || (policy::reserved(&intent.relative_path)
                && !(intent.relative_path == policy::RECEIPT && intent.kind == CaptureKind::File))
            || paths
                .insert(
                    intent.relative_path.as_str(),
                    (intent.kind, intent.max_bytes),
                )
                .is_some()
        {
            return Err(bad());
        }
        let upper = if intent.relative_path == "manifest.json" {
            256 * 1024
        } else if intent.relative_path == policy::RECEIPT {
            package::CONTROL_LIMIT
        } else {
            64 * 1024 * 1024
        };
        if (intent.kind == CaptureKind::Directory && intent.max_bytes != 0)
            || (intent.kind == CaptureKind::File
                && (intent.max_bytes == 0 || intent.max_bytes > upper))
        {
            return Err(bad());
        }
        let mut ancestor = Path::new(&intent.relative_path).parent();
        while let Some(parent) = ancestor {
            if parent.as_os_str().is_empty() {
                break;
            }
            if paths.get(parent.to_str().ok_or_else(bad)?) != Some(&(CaptureKind::Directory, 0)) {
                return Err(bad());
            }
            ancestor = parent.parent();
        }
    }
    if j.phase != JournalPhase::Capturing && j.operation != JournalOperation::Remove {
        for file in &j.candidate_files {
            if !paths
                .get(file.relative_path.as_str())
                .is_some_and(|(kind, max_bytes)| {
                    *kind == CaptureKind::File && file.bytes <= *max_bytes
                })
            {
                return Err(bad());
            }
        }
        for dir in &j.candidate_directories {
            if paths.get(dir.as_str()) != Some(&(CaptureKind::Directory, 0)) {
                return Err(bad());
            }
        }
    }
    match (j.operation, j.phase) {
        (JournalOperation::Remove, JournalPhase::Capturing | JournalPhase::Published) => {
            return Err(bad())
        }
        (JournalOperation::Install | JournalOperation::Update, JournalPhase::RemoveCommitted) => {
            return Err(bad())
        }
        _ => {}
    }
    if j.operation == JournalOperation::Remove
        && (j.candidate_digest.is_some()
            || !j.candidate_files.is_empty()
            || !j.candidate_directories.is_empty()
            || !j.capture_intents.is_empty())
    {
        return Err(bad());
    }
    if j.operation == JournalOperation::Install
        && (!j.backup_files.is_empty() || !j.backup_directories.is_empty())
    {
        return Err(bad());
    }
    if j.operation != JournalOperation::Remove
        && j.phase != JournalPhase::Capturing
        && j.candidate_digest.is_none()
    {
        return Err(bad());
    }
    Ok(())
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), PluginError> {
    let bytes = manifest::record_bytes(value)?;
    let mut file = policy::create_new(path)?;
    file.write_all(&bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}
fn children(path: &Path) -> Result<Vec<String>, PluginError> {
    policy::check_path(path)?;
    let mut out = vec![];
    for entry in fs::read_dir(path).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name().into_string().map_err(|_| bad())?;
        if ![
            policy::OWNER,
            policy::JOURNAL,
            policy::JOURNAL_NEXT,
            "stage",
            "backup",
        ]
        .contains(&name.as_str())
        {
            return Err(bad());
        }
        policy::check_path(&entry.path()).map_err(|_| bad())?;
        out.push(name);
    }
    Ok(out)
}
impl Transaction {
    pub(crate) fn start(
        plugins: &Path,
        id: &str,
        operation: JournalOperation,
        backup: TreeIndex,
    ) -> Result<Self, PluginError> {
        let name = format!("{}{}", policy::TXN_PREFIX, uuid::Uuid::new_v4().simple());
        let path = plugins.join(&name);
        policy::check_path(&path)?;
        fs::create_dir(&path).map_err(io_error)?;
        let journal = TransactionJournal {
            schema_version: 1,
            transaction: name.clone(),
            plugin_id: id.into(),
            operation,
            phase: if operation == JournalOperation::Remove {
                JournalPhase::Prepared
            } else {
                JournalPhase::Capturing
            },
            candidate_digest: None,
            backup_files: backup.files,
            backup_directories: backup.directories,
            candidate_files: vec![],
            candidate_directories: vec![],
            capture_intents: vec![],
        };
        write_json(
            &path.join(policy::OWNER),
            &TransactionOwner {
                schema_version: 1,
                transaction: name,
            },
        )?;
        write_json(&path.join(policy::JOURNAL), &journal)?;
        if operation != JournalOperation::Remove {
            fs::create_dir(path.join("stage")).map_err(io_error)?;
        }
        Ok(Self { path, journal })
    }
    pub(crate) fn save(&self) -> Result<(), PluginError> {
        validate(&self.journal, &self.journal.transaction)?;
        children(&self.path)?;
        let next = self.path.join(policy::JOURNAL_NEXT);
        if policy::exists(&next)? {
            return Err(bad());
        }
        write_json(&next, &self.journal)?;
        let current = self.path.join(policy::JOURNAL);
        policy::check_path(&current)?;
        fs::rename(&next, &current).map_err(io_error)
    }
    pub(crate) fn set_plan(&mut self, mut plan: Vec<CaptureIntent>) -> Result<(), PluginError> {
        plan.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        self.journal.capture_intents = plan;
        self.save()
    }
    pub(crate) fn intent(
        &self,
        relative: &str,
        kind: CaptureKind,
        max_bytes: u64,
    ) -> Result<(), PluginError> {
        if self
            .journal
            .capture_intents
            .binary_search_by(|i| i.relative_path.as_str().cmp(relative))
            .ok()
            .is_some_and(|at| {
                let i = &self.journal.capture_intents[at];
                i.kind == kind && i.max_bytes == max_bytes
            })
        {
            Ok(())
        } else {
            Err(bad())
        }
    }
    pub(crate) fn prepare(&mut self, digest: String) -> Result<(), PluginError> {
        let stage = self.path.join("stage");
        let index = package::scan(&stage, ScanMode::Backup, None, None, None)?;
        self.journal.candidate_digest = Some(digest);
        self.journal.candidate_files = index.files;
        self.journal.candidate_directories = index.directories;
        self.journal.phase = JournalPhase::Prepared;
        self.save()
    }
    pub(crate) fn phase(&mut self, phase: JournalPhase) -> Result<(), PluginError> {
        self.journal.phase = phase;
        self.save()
    }
    pub(crate) fn abort_capture(&self) -> Result<(), PluginError> {
        verify_capture(&self.path.join("stage"), &self.journal)?;
        delete_capture(&self.path, &self.journal)
    }
}
fn load(path: &Path) -> Result<Transaction, PluginError> {
    let names = children(path)?;
    if !names.iter().any(|n| n == policy::OWNER) || !names.iter().any(|n| n == policy::JOURNAL) {
        return Err(bad());
    }
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(bad)?;
    let owner: TransactionOwner = manifest::strict_record(
        &manifest::bounded_text(&path.join(policy::OWNER), 1024).map_err(|_| bad())?,
    )?;
    if owner.schema_version != 1 || owner.transaction != name || !valid_basename(name) {
        return Err(bad());
    }
    let mut journal: TransactionJournal = manifest::strict_record(
        &manifest::bounded_text(&path.join(policy::JOURNAL), crate::CONTROL_RECORD_MAX_BYTES)
            .map_err(|_| bad())?,
    )?;
    validate(&journal, name)?;
    if names.iter().any(|n| n == policy::JOURNAL_NEXT) {
        let next: TransactionJournal = manifest::strict_record(
            &manifest::bounded_text(
                &path.join(policy::JOURNAL_NEXT),
                crate::CONTROL_RECORD_MAX_BYTES,
            )
            .map_err(|_| bad())?,
        )?;
        validate(&next, name)?;
        if next.plugin_id != journal.plugin_id
            || next.operation != journal.operation
            || next.backup_files != journal.backup_files
            || next.backup_directories != journal.backup_directories
            || !next.capture_intents.starts_with(&journal.capture_intents)
        {
            return Err(bad());
        }
        if journal.phase != JournalPhase::Capturing
            && (next.candidate_digest != journal.candidate_digest
                || next.candidate_files != journal.candidate_files
                || next.candidate_directories != journal.candidate_directories
                || next.capture_intents != journal.capture_intents)
        {
            return Err(bad());
        }
        if journal.phase == JournalPhase::Capturing
            && next.phase != JournalPhase::Capturing
            && next.capture_intents != journal.capture_intents
        {
            return Err(bad());
        }
        let transition = next.phase == journal.phase
            || matches!(
                (journal.phase, next.phase),
                (JournalPhase::Capturing, JournalPhase::Prepared)
                    | (JournalPhase::Prepared, JournalPhase::OldMoved)
                    | (
                        JournalPhase::Prepared | JournalPhase::OldMoved,
                        JournalPhase::Published
                            | JournalPhase::RemoveCommitted
                            | JournalPhase::Restoring
                    )
            );
        if !transition {
            return Err(bad());
        }
        policy::check_path(&path.join(policy::JOURNAL_NEXT))?;
        policy::check_path(&path.join(policy::JOURNAL))?;
        fs::rename(path.join(policy::JOURNAL_NEXT), path.join(policy::JOURNAL))
            .map_err(io_error)?;
        journal = next;
    }
    Ok(Transaction {
        path: path.into(),
        journal,
    })
}
fn verify_tree(
    path: &Path,
    files: &[PackageFile],
    dirs: &[String],
    subset: bool,
) -> Result<(), PluginError> {
    let actual = package::scan(path, ScanMode::Backup, None, None, None).map_err(|_| bad())?;
    let okay = if subset {
        actual.files.iter().all(|f| {
            files
                .binary_search_by(|known| known.relative_path.cmp(&f.relative_path))
                .ok()
                .is_some_and(|at| files[at] == *f)
        }) && actual
            .directories
            .iter()
            .all(|d| dirs.binary_search(d).is_ok())
    } else {
        actual.files == files && actual.directories == dirs
    };
    if okay {
        Ok(())
    } else {
        Err(bad())
    }
}
fn verify_capture(stage: &Path, j: &TransactionJournal) -> Result<(), PluginError> {
    if !policy::exists(stage).map_err(|_| bad())? {
        return Ok(());
    }
    if !fs::symlink_metadata(stage).map_err(|_| bad())?.is_dir() {
        return Err(bad());
    }
    let allowed: std::collections::HashMap<_, _> = j
        .capture_intents
        .iter()
        .map(|i| (i.relative_path.as_str(), i))
        .collect();
    let mut pending = vec![String::new()];
    let mut total = 0u64;
    while let Some(relative) = pending.pop() {
        let dir = stage.join(&relative);
        policy::check_path(&dir).map_err(|_| bad())?;
        for entry in fs::read_dir(&dir).map_err(|_| bad())? {
            let entry = entry.map_err(|_| bad())?;
            let name = entry.file_name().into_string().map_err(|_| bad())?;
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let intent = allowed.get(path.as_str()).ok_or_else(bad)?;
            policy::check_path(&entry.path()).map_err(|_| bad())?;
            let meta = fs::symlink_metadata(entry.path()).map_err(|_| bad())?;
            if policy::is_link(&meta) {
                return Err(bad());
            }
            if meta.is_dir() && intent.kind == CaptureKind::Directory {
                pending.push(path);
            } else if meta.is_file() && intent.kind == CaptureKind::File {
                // Ownership cleanup needs bounded regular files, not verified complete hashes.
                let file = policy::open_read(&entry.path()).map_err(|_| bad())?;
                let bytes = file.metadata().map_err(|_| bad())?.len();
                if bytes > intent.max_bytes {
                    return Err(bad());
                }
                total = total.checked_add(bytes).ok_or_else(bad)?;
                if total > 256 * 1024 * 1024 + crate::CONTROL_RECORD_MAX_BYTES {
                    return Err(bad());
                }
            } else {
                return Err(bad());
            }
        }
    }
    Ok(())
}
fn delete_tree(path: &Path) -> Result<(), PluginError> {
    policy::check_path(path)?;
    fs::remove_dir_all(path).map_err(io_error)
}
fn delete_controls(path: &Path) -> Result<(), PluginError> {
    let names = children(path)?;
    if names
        .iter()
        .any(|n| n == "stage" || n == "backup" || n == policy::JOURNAL_NEXT)
    {
        return Err(bad());
    }
    for name in [policy::JOURNAL, policy::OWNER] {
        let file = path.join(name);
        policy::check_path(&file)?;
        fs::remove_file(file).map_err(io_error)?;
    }
    policy::check_path(path)?;
    fs::remove_dir(path).map_err(io_error)
}
fn delete_capture(path: &Path, j: &TransactionJournal) -> Result<(), PluginError> {
    children(path)?;
    let stage = path.join("stage");
    if policy::exists(&path.join("backup"))? {
        return Err(bad());
    }
    if policy::exists(&stage)? {
        verify_capture(&stage, j)?;
        delete_tree(&stage)?;
    }
    delete_controls(path)
}
fn cleanup(txn: &Transaction) -> Result<(), PluginError> {
    children(&txn.path)?;
    let stage = txn.path.join("stage");
    let backup = txn.path.join("backup");
    // Verify every object before deleting any; subset accepts only prior authorized partial cleanup.
    if policy::exists(&stage)? {
        verify_tree(
            &stage,
            &txn.journal.candidate_files,
            &txn.journal.candidate_directories,
            true,
        )?;
    }
    if policy::exists(&backup)? {
        verify_tree(
            &backup,
            &txn.journal.backup_files,
            &txn.journal.backup_directories,
            true,
        )?;
    }
    if policy::exists(&stage)? {
        delete_tree(&stage)?;
    }
    if policy::exists(&backup)? {
        delete_tree(&backup)?;
    }
    delete_controls(&txn.path)
}
fn candidate(txn: &Transaction, target: &Path) -> Result<(), PluginError> {
    let receipt = package::verify_receipt(target, &txn.journal.plugin_id).map_err(|_| bad())?;
    if txn.journal.candidate_digest.as_deref() != Some(&receipt.digest) {
        return Err(bad());
    }
    verify_tree(
        target,
        &txn.journal.candidate_files,
        &txn.journal.candidate_directories,
        false,
    )
}
pub(crate) fn recover(pm: &PackageManager, plugins: &Path) -> Result<(), PluginError> {
    if !policy::exists(plugins)? {
        return Ok(());
    }
    let mut paths = vec![];
    for entry in fs::read_dir(plugins).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(policy::TXN_PREFIX)
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    for path in paths {
        let mut txn = load(&path)?;
        let j = txn.journal.clone();
        // A forged or stale journal must not bypass the current builtin catalog.
        pm.reserved(&j.plugin_id).map_err(|_| bad())?;
        let target = plugins.join(&j.plugin_id);
        policy::check_path(&target).map_err(|_| bad())?;
        let _lease = pm.exclusive_lease(&j.plugin_id)?;
        let backup = path.join("backup");
        let stage = path.join("stage");
        let has_target = policy::exists(&target)?;
        let has_backup = policy::exists(&backup)?;
        let has_stage = policy::exists(&stage)?;
        if j.phase == JournalPhase::Capturing {
            if has_backup {
                return Err(bad());
            }
            if j.operation == JournalOperation::Update {
                if !has_target {
                    return Err(bad());
                }
                verify_tree(&target, &j.backup_files, &j.backup_directories, false)?;
            } else if has_target {
                return Err(bad());
            }
            txn.abort_capture()?;
            continue;
        }
        if j.phase == JournalPhase::RemoveCommitted {
            if has_target || has_stage {
                return Err(bad());
            }
            cleanup(&txn)?;
            continue;
        }
        if j.operation != JournalOperation::Remove
            && has_target
            && (!has_stage || j.phase == JournalPhase::Published)
            && j.phase != JournalPhase::Restoring
        {
            candidate(&txn, &target)?;
            cleanup(&txn)?;
            continue;
        }
        if j.phase == JournalPhase::Published {
            return Err(bad());
        }
        // Precommit rollback: backup can move only onto an absent validated target.
        if has_backup {
            if has_target {
                return Err(bad());
            }
            verify_tree(&backup, &j.backup_files, &j.backup_directories, false)?;
            if has_stage {
                verify_tree(&stage, &j.candidate_files, &j.candidate_directories, false)?;
            }
            txn.phase(JournalPhase::Restoring)?;
            policy::check_path(&target)?;
            policy::check_path(&backup)?;
            fs::rename(&backup, &target).map_err(io_error)?;
            cleanup(&txn)?;
        } else if j.operation == JournalOperation::Install {
            if has_target {
                return Err(bad());
            }
            cleanup(&txn)?;
        } else {
            if !has_target {
                return Err(bad());
            }
            verify_tree(&target, &j.backup_files, &j.backup_directories, false)?;
            cleanup(&txn)?;
        }
    }
    Ok(())
}
pub(crate) fn publish(
    pm: &PackageManager,
    mut txn: Transaction,
    target: &Path,
) -> Result<(), PluginError> {
    let result = (|| {
        if txn.journal.operation != JournalOperation::Install {
            verify_tree(
                target,
                &txn.journal.backup_files,
                &txn.journal.backup_directories,
                false,
            )?;
            let backup = txn.path.join("backup");
            policy::check_path(target)?;
            policy::check_path(&backup)?;
            fs::rename(target, &backup).map_err(io_error)?;
            txn.phase(JournalPhase::OldMoved)?;
        } else if policy::exists(target)? {
            return Err(bad());
        }
        if txn.journal.operation == JournalOperation::Remove {
            txn.phase(JournalPhase::RemoveCommitted)?;
        } else {
            let stage = txn.path.join("stage");
            candidate(&txn, &stage)?;
            policy::check_path(&stage)?;
            policy::check_path(target)?;
            if policy::exists(target)? {
                return Err(bad());
            }
            fs::rename(&stage, target).map_err(io_error)?;
            txn.phase(JournalPhase::Published)?;
            candidate(&txn, target)?;
        }
        cleanup(&txn)
    })();
    // The caller retains the exclusive lease. Recover inline without trying to reacquire it.
    if let Err(error) = result {
        return publication_failure(&mut txn, target, error);
    }
    let _ = pm;
    Ok(())
}

fn publication_failure(
    txn: &mut Transaction,
    target: &Path,
    mut error: PluginError,
) -> Result<(), PluginError> {
    // Set uncertainty BEFORE any fallible probe. A probe/rollback error must never escape
    // with a newly constructed error's default outcome_known=true after an old-target move.
    error.outcome_known = false;
    if matches!(
        txn.journal.phase,
        JournalPhase::Published | JournalPhase::RemoveCommitted
    ) {
        return Err(error.with_plugin_id(&txn.journal.plugin_id));
    }
    let verified_rollback = (|| -> Result<(), PluginError> {
        let backup = txn.path.join("backup");
        let has_target = policy::exists(target)?;
        let has_backup = policy::exists(&backup)?;
        if has_target {
            // Never remove/overwrite a foreign target or discard its adjacent original backup.
            if has_backup || txn.journal.operation == JournalOperation::Install {
                return Err(bad());
            }
            verify_tree(
                target,
                &txn.journal.backup_files,
                &txn.journal.backup_directories,
                false,
            )?;
        } else if has_backup {
            verify_tree(
                &backup,
                &txn.journal.backup_files,
                &txn.journal.backup_directories,
                false,
            )?;
            txn.phase(JournalPhase::Restoring)?;
            policy::check_path(&backup)?;
            policy::check_path(target)?;
            if policy::exists(target)? {
                return Err(bad());
            }
            fs::rename(&backup, target).map_err(io_error)?;
            verify_tree(
                target,
                &txn.journal.backup_files,
                &txn.journal.backup_directories,
                false,
            )?;
        } else if txn.journal.operation != JournalOperation::Install {
            return Err(bad());
        }
        cleanup(txn)?;
        // Cleanup and restoration are complete. Prove the final original image (or the
        // original absence for a fresh install), not just a successful rename/existence check.
        if txn.journal.operation == JournalOperation::Install {
            if policy::exists(target)? {
                return Err(bad());
            }
        } else {
            verify_tree(
                target,
                &txn.journal.backup_files,
                &txn.journal.backup_directories,
                false,
            )?;
        }
        Ok(())
    })();
    if verified_rollback.is_ok() {
        error.outcome_known = true;
    }
    Err(error.with_plugin_id(&txn.journal.plugin_id))
}

#[cfg(test)]
mod publication_failure_tests {
    use super::*;

    const OLD_MANIFEST: &str = r#"{"id":"code","name":"Old fixture","version":"old","api":"1","steps":[{"op":"enable-plugin","target":"news"}]}"#;

    fn installed() -> (tempfile::TempDir, PackageManager, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.json");
        fs::write(&source, OLD_MANIFEST).unwrap();
        let pm = PackageManager::new(
            root.path().into(),
            vec![],
            "0.1.0".into(),
            "windows-x86_64".into(),
        );
        let target = pm.install(&source, true).unwrap().installed_path;
        (root, pm, target)
    }
    fn transaction(pm: &PackageManager, target: &Path, moved: bool) -> Transaction {
        let (_, plugins) = pm.paths().unwrap();
        let original = package::scan(target, ScanMode::Backup, None, None, None).unwrap();
        let mut txn =
            Transaction::start(&plugins, "code", JournalOperation::Remove, original).unwrap();
        if moved {
            let backup = txn.path.join("backup");
            policy::check_path(target).unwrap();
            policy::check_path(&backup).unwrap();
            fs::rename(target, &backup).unwrap();
            txn.phase(JournalPhase::OldMoved).unwrap();
        }
        txn
    }
    fn actual_io_error(root: &Path) -> PluginError {
        let obstruction = root.join("obstruction");
        fs::create_dir(&obstruction).unwrap();
        policy::create_new(&obstruction).unwrap_err()
    }
    fn target_link(target: &Path, destination: &Path) {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(destination, target).unwrap();
        }
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(target)
                .arg(destination)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction fixture failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn inline_old_moved_foreign_target_preserves_both_and_reports_unknown() {
        let (root, pm, target) = installed();
        let _install = pm.install_lock().unwrap();
        let _lease = pm.exclusive_lease("code").unwrap();
        let mut txn = transaction(&pm, &target, true);
        let old_receipt = fs::read(txn.path.join("backup").join(policy::RECEIPT)).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("foreign"), b"foreign target").unwrap();
        let error =
            publication_failure(&mut txn, &target, actual_io_error(root.path())).unwrap_err();
        assert!(
            !error.outcome_known,
            "old backup is stranded beside a foreign target"
        );
        assert_eq!(fs::read(target.join("foreign")).unwrap(), b"foreign target");
        assert_eq!(
            fs::read(txn.path.join("backup/manifest.json")).unwrap(),
            OLD_MANIFEST.as_bytes()
        );
        assert_eq!(
            fs::read(txn.path.join("backup").join(policy::RECEIPT)).unwrap(),
            old_receipt
        );
    }
    #[test]
    fn inline_old_moved_probe_error_cannot_escape_with_known_outcome() {
        let (root, pm, target) = installed();
        let _install = pm.install_lock().unwrap();
        let _lease = pm.exclusive_lease("code").unwrap();
        let mut txn = transaction(&pm, &target, true);
        let sentinel = tempfile::tempdir().unwrap();
        fs::write(sentinel.path().join("sentinel"), b"unrelated").unwrap();
        target_link(&target, sentinel.path());
        assert!(
            policy::exists(&target).is_err(),
            "fixture must trigger a real path probe failure"
        );
        let error =
            publication_failure(&mut txn, &target, actual_io_error(root.path())).unwrap_err();
        assert!(
            !error.outcome_known,
            "a post-move existence probe lost uncertainty"
        );
        assert_eq!(
            fs::read(txn.path.join("backup/manifest.json")).unwrap(),
            OLD_MANIFEST.as_bytes()
        );
        assert_eq!(
            fs::read(sentinel.path().join("sentinel")).unwrap(),
            b"unrelated"
        );
        assert!(fs::symlink_metadata(&target).is_ok());
    }
    #[test]
    fn inline_changed_unmoved_original_is_not_a_known_rollback() {
        let (root, pm, target) = installed();
        let _install = pm.install_lock().unwrap();
        let _lease = pm.exclusive_lease("code").unwrap();
        let mut txn = transaction(&pm, &target, false);
        fs::write(target.join("manifest.json"), b"changed original").unwrap();
        let error =
            publication_failure(&mut txn, &target, actual_io_error(root.path())).unwrap_err();
        assert!(
            !error.outcome_known,
            "target existence is not proof of the unchanged original"
        );
        assert_eq!(
            fs::read(target.join("manifest.json")).unwrap(),
            b"changed original"
        );
        assert!(txn.path.exists());
    }
    #[test]
    fn inline_verified_unmoved_original_can_report_known_failure() {
        let (root, pm, target) = installed();
        let _install = pm.install_lock().unwrap();
        let _lease = pm.exclusive_lease("code").unwrap();
        let mut txn = transaction(&pm, &target, false);
        let error =
            publication_failure(&mut txn, &target, actual_io_error(root.path())).unwrap_err();
        assert!(error.outcome_known);
        assert_eq!(
            fs::read(target.join("manifest.json")).unwrap(),
            OLD_MANIFEST.as_bytes()
        );
        assert!(!txn.path.exists());
    }
    #[test]
    fn inline_verified_completed_restore_can_report_known_failure() {
        let (root, pm, target) = installed();
        let _install = pm.install_lock().unwrap();
        let _lease = pm.exclusive_lease("code").unwrap();
        let mut txn = transaction(&pm, &target, true);
        let old_receipt = fs::read(txn.path.join("backup").join(policy::RECEIPT)).unwrap();
        let error =
            publication_failure(&mut txn, &target, actual_io_error(root.path())).unwrap_err();
        assert!(error.outcome_known);
        assert_eq!(
            fs::read(target.join("manifest.json")).unwrap(),
            OLD_MANIFEST.as_bytes()
        );
        assert_eq!(fs::read(target.join(policy::RECEIPT)).unwrap(), old_receipt);
        assert!(!txn.path.exists());
    }
}
