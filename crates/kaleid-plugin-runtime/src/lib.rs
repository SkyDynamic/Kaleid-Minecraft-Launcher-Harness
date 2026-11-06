//! Package management only. Installing never launches code or grants authorization.
mod inventory;
mod manifest;
mod package;
mod policy;
mod transaction;

pub use inventory::{InventoryItem, InventoryState};
pub use package::{PackageFile, PackageMetadata, PackagePreview, PackageReceipt};
pub use transaction::{
    CaptureIntent, CaptureKind, JournalOperation, JournalPhase, TransactionJournal,
    TransactionOwner,
};

use kaleid_api::{PluginError, PluginErrorCode};
use package::{ScanMode, TreeIndex};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

/// Receipt/journal byte capacity, independent of the 256 MiB source payload limit.
pub const CONTROL_RECORD_MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PackageManager {
    data_root: PathBuf,
    builtin_ids: Vec<String>,
    app_version: String,
    platform: String,
}
/// Shared read lease. Independent desktop/CLI runtimes may coexist; any reader blocks mutation.
/// It neither grants authorization nor verifies an executable or creates a singleton runtime.
#[derive(Debug)]
pub struct PackageLease {
    file: File,
}
impl Drop for PackageLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}
#[derive(Debug)]
struct InstallLock {
    file: File,
}
impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}
impl PackageManager {
    pub fn new(
        data_root: PathBuf,
        builtin_ids: Vec<String>,
        app_version: String,
        platform: String,
    ) -> Self {
        Self {
            data_root,
            builtin_ids,
            app_version,
            platform,
        }
    }
    fn paths(&self) -> Result<(PathBuf, PathBuf), PluginError> {
        let root = policy::absolute(&self.data_root)?;
        policy::check_path(&root)?;
        let plugins = root.join("plugins");
        policy::check_path(&plugins)?;
        Ok((root, plugins))
    }
    fn install_lock(&self) -> Result<InstallLock, PluginError> {
        let (root, plugins) = self.paths()?;
        policy::ensure_dir(&root)?;
        let file = policy::lock_file(&root.join(".plugin-install.lock"))?;
        fs2::FileExt::lock_exclusive(&file).map_err(io_error)?;
        policy::ensure_dir(&plugins)?;
        Ok(InstallLock { file })
    }
    fn reserved(&self, id: &str) -> Result<(), PluginError> {
        policy::validate_id(id)?;
        if self.builtin_ids.iter().any(|builtin| builtin == id) {
            return Err(PluginError::new(
                PluginErrorCode::Unauthorized,
                "第三方不能覆盖或移除内置插件 (reserved builtin id)",
            )
            .with_plugin_id(id));
        }
        Ok(())
    }
    fn lease_file(&self, id: &str) -> Result<File, PluginError> {
        policy::validate_id(id)?;
        let (root, _) = self.paths()?;
        policy::ensure_dir(&root)?;
        let locks = root.join(".plugin-runtime-locks");
        policy::ensure_dir(&locks)?;
        policy::lock_file(&locks.join(format!("{id}.lock")))
    }
    pub(crate) fn exclusive_lease(&self, id: &str) -> Result<PackageLease, PluginError> {
        let file = self.lease_file(id)?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            {
                PluginError::new(
                    PluginErrorCode::Busy,
                    "Package is in use by a runtime lease",
                )
                .with_plugin_id(id)
            } else {
                io_error(error).with_plugin_id(id)
            }
        })?;
        Ok(PackageLease { file })
    }
    pub fn acquire_runtime_lease(&self, id: &str) -> Result<PackageLease, PluginError> {
        policy::validate_id(id)?;
        let _install = self.install_lock()?;
        let (_, plugins) = self.paths()?;
        transaction::recover(self, &plugins)?;
        let target = plugins.join(id);
        policy::check_path(&target)?;
        if !policy::exists(&target)?
            || !std::fs::symlink_metadata(&target)
                .map_err(io_error)?
                .is_dir()
        {
            return Err(
                PluginError::new(PluginErrorCode::Io, "Package is not installed")
                    .with_plugin_id(id),
            );
        }
        let file = self.lease_file(id)?;
        fs2::FileExt::try_lock_shared(&file).map_err(|e| {
            PluginError::new(
                PluginErrorCode::Busy,
                format!("Package mutation prevents runtime lease: {e}"),
            )
            .with_plugin_id(id)
        })?;
        Ok(PackageLease { file })
    }
    /// Advisory, read-only metadata. No grant, publication, recovery, or source execution occurs.
    pub fn preview(&self, source: &Path) -> Result<PackagePreview, PluginError> {
        let captured = package::capture(source, None, &self.app_version, &self.platform)?;
        self.reserved(captured.manifest.id())?;
        let (_, plugins) = self.paths()?;
        Ok(PackagePreview {
            installed_path: plugins.join(captured.manifest.id()),
            requires_unsigned_consent: policy::signature(&captured.manifest).is_none(),
            signature_unsupported: policy::signature(&captured.manifest).is_some(),
            contains_code: package::contains_code(&captured.manifest),
            manifest: captured.manifest,
            digest: captured.digest,
            files: captured.index.files,
        })
    }
    pub fn install(
        &self,
        source: &Path,
        allow_unsigned: bool,
    ) -> Result<PackageMetadata, PluginError> {
        self.mutate(source, allow_unsigned, false, false)
    }
    /// Compatibility wrapper entry: checks the captured API before any package publication.
    pub fn install_api1(
        &self,
        source: &Path,
        allow_unsigned: bool,
    ) -> Result<PackageMetadata, PluginError> {
        self.mutate(source, allow_unsigned, false, true)
    }
    pub fn update(
        &self,
        source: &Path,
        allow_unsigned: bool,
    ) -> Result<PackageMetadata, PluginError> {
        self.mutate(source, allow_unsigned, true, false)
    }
    fn mutate(
        &self,
        source: &Path,
        allow_unsigned: bool,
        updating: bool,
        api1_only: bool,
    ) -> Result<PackageMetadata, PluginError> {
        // Manifest bytes are read once, retained, validated and copied verbatim. Assets stream
        // into the staged snapshot once; all subsequent checks/publication use that snapshot.
        let prepared = package::prepare_source(source, &self.app_version, &self.platform)?;
        if api1_only && !matches!(prepared.manifest, kaleid_api::PluginManifest::V1(_)) {
            return Err(PluginError::new(
                PluginErrorCode::UnsupportedApi,
                "API 2 requires the typed PackageManager install interface",
            )
            .with_plugin_id(prepared.manifest.id()));
        }
        let id = prepared.manifest.id().to_string();
        self.reserved(&id)?;
        policy::trust(&prepared.manifest, allow_unsigned)?;
        let _install = self.install_lock()?;
        let (_, plugins) = self.paths()?;
        transaction::recover(self, &plugins)?;
        let items = inventory::read(self, &plugins)?;
        let target = plugins.join(&id);
        let exists = policy::exists(&target)?;
        if exists && !updating {
            return Err(PluginError::new(
                PluginErrorCode::AlreadyInstalled,
                "Package already installed; explicit update required",
            )
            .with_plugin_id(&id));
        }
        if !exists && updating {
            return Err(PluginError::new(
                PluginErrorCode::Io,
                "Update requires an installed third-party package",
            )
            .with_plugin_id(&id));
        }
        inventory::conflicts(&prepared.manifest, &items, updating)?;
        let _lease = self.exclusive_lease(&id)?;
        let backup = if updating {
            package::scan(&target, ScanMode::Backup, None, None, None)?
        } else {
            TreeIndex {
                files: vec![],
                directories: vec![],
            }
        };
        let plan = package::capture_plan(&prepared)?;
        let mut txn = transaction::Transaction::start(
            &plugins,
            &id,
            if updating {
                JournalOperation::Update
            } else {
                JournalOperation::Install
            },
            backup,
        )?;
        txn.set_plan(plan)?;
        let stage = txn.path.join("stage");
        let captured = match package::capture_prepared(prepared, Some(&stage), Some(&mut txn)) {
            Ok(captured) => captured,
            Err(mut error) => {
                if txn.abort_capture().is_err() {
                    error.outcome_known = false;
                }
                return Err(error.with_plugin_id(&id));
            }
        };
        if let Err(mut error) = package::write_receipt(&stage, &captured, &mut txn) {
            if txn.abort_capture().is_err() {
                error.outcome_known = false;
            }
            return Err(error.with_plugin_id(&id));
        }
        package::verify_receipt(&stage, &id)?;
        txn.prepare(captured.digest.clone())?;
        let result = PackageMetadata {
            contains_code: package::contains_code(&captured.manifest),
            manifest: captured.manifest,
            installed_path: target.clone(),
            digest: captured.digest,
            files: captured.index.files,
        };
        transaction::publish(self, txn, &target)?;
        Ok(result)
    }
    /// Corrupt manifests remain removable; no manifest parse is needed to identify the folder.
    pub fn remove(&self, id: &str) -> Result<(), PluginError> {
        self.reserved(id)?;
        let _install = self.install_lock()?;
        let (_, plugins) = self.paths()?;
        transaction::recover(self, &plugins)?;
        let target = plugins.join(id);
        policy::check_path(&target)?;
        if !policy::exists(&target)?
            || !std::fs::symlink_metadata(&target)
                .map_err(io_error)?
                .is_dir()
        {
            return Err(
                PluginError::new(PluginErrorCode::Io, "Package is not installed")
                    .with_plugin_id(id),
            );
        }
        let _lease = self.exclusive_lease(id)?;
        let backup = package::scan(&target, ScanMode::Backup, None, None, None)?;
        let txn = transaction::Transaction::start(&plugins, id, JournalOperation::Remove, backup)?;
        transaction::publish(self, txn, &target)
    }
    /// Includes builtin, valid, incompatible and invalid installed folders, regardless of activity.
    /// Installer recovery runs under the cross-process lock before inventory is inspected.
    pub fn list(&self) -> Result<Vec<InventoryItem>, PluginError> {
        let _install = self.install_lock()?;
        let (_, plugins) = self.paths()?;
        transaction::recover(self, &plugins)?;
        inventory::read(self, &plugins)
    }
}
fn io_error(error: impl std::fmt::Display) -> PluginError {
    PluginError::new(
        PluginErrorCode::Io,
        format!("Package filesystem operation failed: {error}"),
    )
}
