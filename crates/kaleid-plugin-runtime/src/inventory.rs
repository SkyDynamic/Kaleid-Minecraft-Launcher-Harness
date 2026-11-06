use crate::{
    io_error, manifest,
    package::{self, ScanMode},
    policy, PackageFile, PackageManager,
};
use kaleid_api::{ConflictDetail, PluginError, PluginErrorCode, PluginManifest};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryState {
    Builtin,
    Ready,
    Incompatible,
    Invalid,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItem {
    pub id: String,
    pub installed_path: Option<PathBuf>,
    pub manifest: Option<PluginManifest>,
    pub state: InventoryState,
    pub error: Option<PluginError>,
    pub digest: Option<String>,
    pub files: Vec<PackageFile>,
    pub contains_code: bool,
    pub receipt_verified: bool,
}
fn installed(pm: &PackageManager, id: String, path: PathBuf) -> InventoryItem {
    let mut item = InventoryItem {
        id,
        installed_path: Some(path.clone()),
        manifest: None,
        state: InventoryState::Invalid,
        error: None,
        digest: None,
        files: vec![],
        contains_code: false,
        receipt_verified: false,
    };
    let result = (|| {
        policy::validate_id(&item.id)?;
        policy::check_path(&path)?;
        let meta = fs::symlink_metadata(&path).map_err(io_error)?;
        if !meta.is_dir() || policy::is_link(&meta) {
            return Err(policy::entry_error("Installed package folder is unsafe"));
        }
        let text = manifest::bounded_text(&path.join("manifest.json"), manifest::MANIFEST_LIMIT)?;
        let m = manifest::intrinsic(&text)?;
        if m.id() != item.id {
            return Err(manifest::record_error().with_plugin_id(&item.id));
        }
        // Preserve intrinsically valid declarations even if assets/receipt are broken.
        item.contains_code = package::contains_code(&m);
        item.manifest = Some(m.clone());
        if policy::exists(&path.join(policy::RECEIPT))? {
            let receipt = package::verify_receipt(&path, &item.id)?;
            item.digest = Some(receipt.digest);
            item.files = receipt.files;
            item.receipt_verified = true;
        } else {
            let index = package::scan(&path, ScanMode::Payload, None, None, None)?;
            package::entries(&m, &index.files, &path)?;
            item.digest = Some(package::digest(&index.files));
            item.files = index.files;
        }
        match policy::compatible(&m, &pm.app_version, &pm.platform) {
            Ok(()) => item.state = InventoryState::Ready,
            Err(e)
                if matches!(
                    e.code,
                    PluginErrorCode::AppVersionMismatch | PluginErrorCode::UnsupportedPlatform
                ) =>
            {
                item.state = InventoryState::Incompatible;
                item.error = Some(e);
            }
            Err(e) => return Err(e),
        }
        Ok(())
    })();
    if let Err(e) = result {
        item.state = InventoryState::Invalid;
        item.error = Some(e.with_plugin_id(&item.id));
    }
    item
}
pub(crate) fn read(pm: &PackageManager, plugins: &Path) -> Result<Vec<InventoryItem>, PluginError> {
    policy::check_path(plugins)?;
    let mut items: Vec<_> = pm
        .builtin_ids
        .iter()
        .map(|id| InventoryItem {
            id: id.clone(),
            installed_path: None,
            manifest: None,
            state: InventoryState::Builtin,
            error: None,
            digest: None,
            files: vec![],
            contains_code: false,
            receipt_verified: false,
        })
        .collect();
    if policy::exists(plugins)? {
        let mut entries = fs::read_dir(plugins)
            .map_err(io_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_error)?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let id = entry.file_name().to_string_lossy().into_owned();
            // Successful recovery removes owned containers. Unrecognized/tampered prefix folders
            // deliberately remain invalid inventory instead of disappearing by name.
            if !policy::validate_id(&id).is_ok() && !id.starts_with(policy::TXN_PREFIX) {
                continue;
            }
            items.push(installed(pm, id, entry.path()));
        }
    }
    items.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then(a.installed_path.cmp(&b.installed_path))
    });
    Ok(items)
}
pub(crate) fn conflicts(
    incoming: &PluginManifest,
    items: &[InventoryItem],
    updating: bool,
) -> Result<(), PluginError> {
    if let Some(invalid) = items.iter().find(|i| i.state == InventoryState::Invalid) {
        return Err(PluginError::new(
            PluginErrorCode::InventoryInvalid,
            format!(
                "Installed inventory `{}` is invalid; repair/remove before installation",
                invalid.id
            ),
        )
        .with_plugin_id(incoming.id()));
    }
    let mut details = vec![];
    for item in items {
        if updating && item.id == incoming.id() && item.state != InventoryState::Builtin {
            continue;
        }
        for c in incoming.conflicts() {
            if c.id == item.id {
                details.push(ConflictDetail {
                    id: c.id.clone(),
                    reason: c.reason.clone(),
                    declared_by: incoming.id().into(),
                });
            }
        }
        if let Some(existing) = &item.manifest {
            for c in existing.conflicts() {
                if c.id == incoming.id() {
                    details.push(ConflictDetail {
                        id: c.id.clone(),
                        reason: c.reason.clone(),
                        declared_by: existing.id().into(),
                    });
                }
            }
        }
    }
    details.sort_by(|a, b| {
        (&a.id, &a.declared_by, &a.reason).cmp(&(&b.id, &b.declared_by, &b.reason))
    });
    details.dedup();
    if details.is_empty() {
        Ok(())
    } else {
        let mut e = PluginError::new(
            PluginErrorCode::Conflict,
            "Package conflicts with installed inventory",
        )
        .with_plugin_id(incoming.id());
        e.conflicts = details;
        Err(e)
    }
}
