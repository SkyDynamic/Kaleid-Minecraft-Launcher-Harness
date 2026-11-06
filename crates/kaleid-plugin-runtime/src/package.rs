use crate::{io_error, manifest, policy};
use kaleid_api::{validate_relative_entry, PluginError, PluginErrorCode, PluginManifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const FILE_LIMIT: u64 = 64 * 1024 * 1024;
const PACKAGE_LIMIT: u64 = 256 * 1024 * 1024;
pub(crate) const CONTROL_LIMIT: u64 = crate::CONTROL_RECORD_MAX_BYTES;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFile {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
    /// POSIX rwx mode when captured on Unix; absent on Windows. No special bits in payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageMetadata {
    pub manifest: PluginManifest,
    pub installed_path: PathBuf,
    pub digest: String,
    pub files: Vec<PackageFile>,
    pub contains_code: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagePreview {
    pub manifest: PluginManifest,
    pub installed_path: PathBuf,
    pub digest: String,
    pub files: Vec<PackageFile>,
    pub contains_code: bool,
    pub requires_unsigned_consent: bool,
    pub signature_unsupported: bool,
}
/// No generic manifest deserialization: original text enters through strict API ingress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageReceipt {
    pub schema_version: u32,
    pub manifest_json: String,
    pub digest: String,
    pub files: Vec<PackageFile>,
    pub contains_code: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeIndex {
    pub files: Vec<PackageFile>,
    pub directories: Vec<String>,
}
#[derive(Clone, Copy)]
pub(crate) enum ScanMode {
    Source,
    Payload,
    Backup,
}
pub(crate) struct Captured {
    pub manifest: PluginManifest,
    pub text: String,
    pub index: TreeIndex,
    pub digest: String,
}
pub(crate) struct PreparedSource {
    pub manifest: PluginManifest,
    pub text: String,
    file: PathBuf,
    unix_mode: Option<u32>,
}
fn too_large() -> PluginError {
    PluginError::new(
        PluginErrorCode::PackageTooLarge,
        "Package exceeds manifest 256 KiB, file 64 MiB, or total 256 MiB limit",
    )
}
#[cfg(test)]
mod mode_tests {
    use super::*;
    fn entry(mode: u32) -> PackageFile {
        PackageFile {
            relative_path: "backend".into(),
            sha256: "0".repeat(64),
            bytes: 0,
            unix_mode: Some(mode),
        }
    }
    #[test]
    fn captured_unix_mode_changes_package_identity() {
        assert_ne!(digest(&[entry(0o755)]), digest(&[entry(0o644)]));
    }
    #[test]
    fn payload_index_refuses_special_mode_bits_but_raw_backup_can_record_them() {
        assert!(!valid_index(&[entry(0o4755)]));
        assert!(valid_tree_index(&[entry(0o4755)], true));
    }
}

pub(crate) fn digest(files: &[PackageFile]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"KaleidPackageIndex\0v2\0");
    hash.update((files.len() as u64).to_be_bytes());
    for f in files {
        hash.update((f.relative_path.len() as u64).to_be_bytes());
        hash.update(f.relative_path.as_bytes());
        hash.update(hex::decode(&f.sha256).expect("validated SHA256 index"));
        hash.update(f.bytes.to_be_bytes());
        match f.unix_mode {
            None => hash.update([0]),
            Some(mode) => {
                hash.update([1]);
                hash.update(mode.to_be_bytes());
            }
        }
    }
    hex::encode(hash.finalize())
}
pub(crate) fn valid_index(index: &[PackageFile]) -> bool {
    valid_tree_index(index, false)
}
pub(crate) fn valid_tree_index(index: &[PackageFile], raw: bool) -> bool {
    let mut previous: Option<&str> = None;
    index.iter().all(|f| {
        let valid = validate_relative_entry(&f.relative_path).is_ok()
            && f.sha256.len() == 64
            && f.sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && (raw || f.bytes <= FILE_LIMIT)
            && f.unix_mode
                .is_none_or(|mode| mode <= if raw { 0o7777 } else { 0o777 })
            && previous.is_none_or(|p| p < f.relative_path.as_str());
        previous = Some(&f.relative_path);
        valid
    })
}
fn stream(
    path: &Path,
    relative: &str,
    dest: Option<&Path>,
    captured: Option<(&[u8], Option<u32>)>,
    total: &mut u64,
    mode: ScanMode,
) -> Result<PackageFile, PluginError> {
    let raw = matches!(mode, ScanMode::Backup);
    let limit = if raw {
        fs::symlink_metadata(path).map_err(io_error)?.len()
    } else if relative == "manifest.json" {
        manifest::MANIFEST_LIMIT
    } else {
        FILE_LIMIT
    };
    let total_limit = if raw { u64::MAX } else { PACKAGE_LIMIT };
    let (mut source, unix_mode): (Box<dyn Read>, Option<u32>) =
        if let Some((bytes, unix_mode)) = captured {
            (Box::new(std::io::Cursor::new(bytes)), unix_mode)
        } else {
            let file = policy::open_read(path)?;
            let unix_mode = policy::file_mode(
                &file.metadata().map_err(io_error)?,
                matches!(mode, ScanMode::Source),
            );
            (Box::new(file), unix_mode)
        };
    let mut output = dest.map(policy::create_new).transpose()?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = source.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        *total += count as u64;
        if bytes > limit || *total > total_limit {
            return Err(too_large());
        }
        hash.update(&buffer[..count]);
        if let Some(output) = output.as_mut() {
            output.write_all(&buffer[..count]).map_err(io_error)?;
        }
    }
    if let Some(output) = output {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = unix_mode {
                output
                    .set_permissions(fs::Permissions::from_mode(policy::portable_unix_mode(mode)))
                    .map_err(io_error)?;
            }
        }
        output.sync_all().map_err(io_error)?;
    }
    Ok(PackageFile {
        relative_path: relative.into(),
        sha256: hex::encode(hash.finalize()),
        bytes,
        unix_mode,
    })
}
fn mapped_relative(
    relative: &str,
    name: &str,
    selected: Option<&str>,
) -> Result<(String, bool), PluginError> {
    let original = if relative.is_empty() {
        name.to_string()
    } else {
        format!("{relative}/{name}")
    };
    validate_relative_entry(&original)?;
    let chosen = selected.is_some_and(|selected| {
        relative.is_empty() && {
            #[cfg(windows)]
            {
                name.to_lowercase() == selected.to_lowercase()
            }
            #[cfg(not(windows))]
            {
                name == selected
            }
        }
    });
    if selected.is_some() {
        if policy::reserved(&original) {
            return Err(policy::entry_error(
                "Source path collides with installer-owned metadata",
            ));
        }
        if relative.is_empty() && name.eq_ignore_ascii_case("manifest.json") && !chosen {
            return Err(policy::entry_error(
                "Selected manifest collides with different root manifest.json",
            ));
        }
    }
    Ok((
        if chosen {
            "manifest.json".into()
        } else {
            original
        },
        chosen,
    ))
}

pub(crate) fn scan(
    root: &Path,
    mode: ScanMode,
    dest: Option<&Path>,
    text: Option<(&str, Option<u32>, &str)>,
    mut transaction: Option<&mut crate::transaction::Transaction>,
) -> Result<TreeIndex, PluginError> {
    policy::check_path(root)?;
    let meta = fs::symlink_metadata(root).map_err(io_error)?;
    if policy::is_link(&meta) || !meta.is_dir() {
        return Err(policy::entry_error(
            "Package root must be a regular directory",
        ));
    }
    let mut index = TreeIndex {
        files: vec![],
        directories: vec![],
    };
    let mut total = 0u64;
    let mut metadata_bytes = 4usize;
    fn walk(
        root: &Path,
        relative: &str,
        mode: ScanMode,
        dest: Option<&Path>,
        text: Option<(&str, Option<u32>, &str)>,
        index: &mut TreeIndex,
        total: &mut u64,
        metadata_bytes: &mut usize,
        transaction: &mut Option<&mut crate::transaction::Transaction>,
    ) -> Result<(), PluginError> {
        let dir = root.join(relative);
        policy::check_path(&dir)?;
        let entries = fs::read_dir(&dir).map_err(io_error)?;
        let mut aliases = std::collections::HashSet::new();
        for entry in entries {
            let entry = entry.map_err(io_error)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| policy::entry_error("Package paths must be UTF-8"))?;
            let (path, chosen) =
                mapped_relative(relative, &name, text.map(|(_, _, selected)| selected))?;
            if !aliases.insert(path.to_lowercase()) {
                return Err(policy::entry_error("Portable path case collision"));
            }
            if matches!(mode, ScanMode::Payload) && path == policy::RECEIPT {
                let _ = policy::open_read(&entry.path())?;
                continue;
            }
            if !matches!(mode, ScanMode::Backup) && policy::reserved(&path) {
                return Err(policy::entry_error(
                    "Package path collides with installer-owned metadata",
                ));
            }
            let meta = fs::symlink_metadata(entry.path()).map_err(io_error)?;
            if policy::is_link(&meta) {
                return Err(policy::entry_error("Symlink/reparse point in package tree"));
            }
            if meta.is_dir() {
                manifest::charge_index(metadata_bytes, &path)?;
                index.directories.push(path.clone());
                if let Some(dest) = dest {
                    if let Some(txn) = transaction.as_deref_mut() {
                        txn.intent(&path, crate::transaction::CaptureKind::Directory, 0)?;
                    }
                    policy::ensure_dir(&dest.join(&path))?;
                }
                walk(
                    root,
                    &path,
                    mode,
                    dest,
                    text,
                    index,
                    total,
                    metadata_bytes,
                    transaction,
                )?;
            } else if meta.is_file() {
                let output = dest.map(|d| d.join(&path));
                if output.is_some() {
                    if let Some(txn) = transaction.as_deref_mut() {
                        txn.intent(
                            &path,
                            crate::transaction::CaptureKind::File,
                            if path == "manifest.json" {
                                manifest::MANIFEST_LIMIT
                            } else {
                                FILE_LIMIT
                            },
                        )?;
                    }
                }
                let file = stream(
                    &entry.path(),
                    &path,
                    output.as_deref(),
                    if chosen {
                        text.map(|(text, unix_mode, _)| (text.as_bytes(), unix_mode))
                    } else {
                        None
                    },
                    total,
                    mode,
                )?;
                manifest::charge_index(metadata_bytes, &file)?;
                index.files.push(file);
            } else {
                return Err(policy::entry_error(
                    "Package entries must be regular files/directories",
                ));
            }
        }
        Ok(())
    }
    walk(
        root,
        "",
        mode,
        dest,
        text,
        &mut index,
        &mut total,
        &mut metadata_bytes,
        &mut transaction,
    )?;
    index
        .files
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    index.directories.sort();
    Ok(index)
}
pub(crate) fn contains_code(manifest: &PluginManifest) -> bool {
    matches!(manifest,PluginManifest::V2(m)if m.entrypoints.frontend.is_some()||m.entrypoints.backend.is_some())
}
pub(crate) fn entries(
    manifest: &PluginManifest,
    files: &[PackageFile],
    root: &Path,
) -> Result<(), PluginError> {
    if let PluginManifest::V2(m) = manifest {
        for entry in m
            .entrypoints
            .frontend
            .iter()
            .chain(m.entrypoints.backend.iter().map(|b| &b.entry))
        {
            if !files.iter().any(|f| &f.relative_path == entry) {
                return Err(policy::entry_error(
                    "Required entrypoint is not a regular file in captured package",
                )
                .with_plugin_id(&m.id));
            }
        }
        if let Some(entry) = &m.entrypoints.frontend {
            let mut prefix = Vec::new();
            policy::open_read(&root.join(entry))?
                .take(4096)
                .read_to_end(&mut prefix)
                .map_err(io_error)?;
            let text = String::from_utf8_lossy(&prefix);
            let text = text.trim_start_matches('\u{feff}').trim_start();
            if !text.starts_with('<') || !text.contains('>') {
                return Err(
                    policy::entry_error("Frontend asset must contain HTML markup")
                        .with_plugin_id(&m.id),
                );
            }
        }
    }
    Ok(())
}
pub(crate) fn capture(
    source: &Path,
    dest: Option<&Path>,
    app: &str,
    platform: &str,
) -> Result<Captured, PluginError> {
    capture_prepared(prepare_source(source, app, platform)?, dest, None)
}
pub(crate) fn prepare_source(
    source: &Path,
    app: &str,
    platform: &str,
) -> Result<PreparedSource, PluginError> {
    let source = policy::absolute(source)?;
    policy::check_path(&source)?;
    let meta = fs::symlink_metadata(&source).map_err(io_error)?;
    let file = if meta.is_dir() {
        source.join("manifest.json")
    } else if meta.is_file() {
        source.clone()
    } else {
        return Err(policy::entry_error(
            "Source must be a directory or manifest file",
        ));
    };
    let (text, unix_mode) = manifest::bounded_text_with_mode(&file, manifest::MANIFEST_LIMIT)?;
    let manifest = manifest::intrinsic(&text)?;
    policy::compatible(&manifest, app, platform)?;
    Ok(PreparedSource {
        manifest,
        text,
        file,
        unix_mode,
    })
}
/// Metadata-only enumeration persists custody for every path before any payload file is created.
/// File contents are not read here; the copy/hash pass is the sole asset content read.
pub(crate) fn capture_plan(
    prepared: &PreparedSource,
) -> Result<Vec<crate::transaction::CaptureIntent>, PluginError> {
    use crate::transaction::{CaptureIntent, CaptureKind};
    let mut plan = vec![];
    let mut budget = 2usize;
    fn add(
        plan: &mut Vec<CaptureIntent>,
        budget: &mut usize,
        relative: String,
        kind: CaptureKind,
        max_bytes: u64,
    ) -> Result<(), PluginError> {
        let intent = CaptureIntent {
            relative_path: relative,
            kind,
            max_bytes,
        };
        manifest::charge_index(budget, &intent)?;
        plan.push(intent);
        Ok(())
    }
    fn walk(
        root: &Path,
        relative: &str,
        plan: &mut Vec<CaptureIntent>,
        budget: &mut usize,
        selected: &str,
    ) -> Result<(), PluginError> {
        let dir = root.join(relative);
        policy::check_path(&dir)?;
        for entry in fs::read_dir(&dir).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| policy::entry_error("Package paths must be UTF-8"))?;
            let (path, _) = mapped_relative(relative, &name, Some(selected))?;
            if policy::reserved(&path) {
                return Err(policy::entry_error(
                    "Package path collides with installer-owned metadata",
                ));
            }
            let meta = fs::symlink_metadata(entry.path()).map_err(io_error)?;
            if policy::is_link(&meta) {
                return Err(policy::entry_error(
                    "Symlink/reparse point in source package",
                ));
            }
            if meta.is_dir() {
                add(plan, budget, path.clone(), CaptureKind::Directory, 0)?;
                walk(root, &path, plan, budget, selected)?;
            } else if meta.is_file() {
                add(
                    plan,
                    budget,
                    path.clone(),
                    CaptureKind::File,
                    if path == "manifest.json" {
                        manifest::MANIFEST_LIMIT
                    } else {
                        FILE_LIMIT
                    },
                )?;
            } else {
                return Err(policy::entry_error(
                    "Package paths must be regular files/directories",
                ));
            }
        }
        Ok(())
    }
    if matches!(prepared.manifest, PluginManifest::V1(_)) {
        add(
            &mut plan,
            &mut budget,
            "manifest.json".into(),
            CaptureKind::File,
            manifest::MANIFEST_LIMIT,
        )?;
    } else {
        let selected = prepared
            .file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| policy::entry_error("Manifest filename must be UTF-8"))?;
        let root = prepared
            .file
            .parent()
            .ok_or_else(|| policy::entry_error("Missing package parent"))?;
        walk(root, "", &mut plan, &mut budget, selected)?;
    }
    add(
        &mut plan,
        &mut budget,
        policy::RECEIPT.into(),
        CaptureKind::File,
        CONTROL_LIMIT,
    )?;
    Ok(plan)
}

pub(crate) fn capture_prepared(
    prepared: PreparedSource,
    dest: Option<&Path>,
    mut transaction: Option<&mut crate::transaction::Transaction>,
) -> Result<Captured, PluginError> {
    let PreparedSource {
        manifest,
        text,
        file,
        unix_mode,
    } = prepared;
    let index = match &manifest {
        PluginManifest::V1(_) => {
            let mut total = 0;
            let target = dest.map(|d| d.join("manifest.json"));
            if let Some(txn) = transaction.as_deref_mut() {
                txn.intent(
                    "manifest.json",
                    crate::transaction::CaptureKind::File,
                    manifest::MANIFEST_LIMIT,
                )?;
            }
            TreeIndex {
                files: vec![stream(
                    &file,
                    "manifest.json",
                    target.as_deref(),
                    Some((text.as_bytes(), unix_mode)),
                    &mut total,
                    ScanMode::Source,
                )?],
                directories: vec![],
            }
        }
        PluginManifest::V2(_) => {
            let selected = file
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| policy::entry_error("Manifest filename must be UTF-8"))?;
            let root = file
                .parent()
                .ok_or_else(|| policy::entry_error("Missing package parent"))?;
            let index = scan(
                root,
                ScanMode::Source,
                dest,
                Some((&text, unix_mode, selected)),
                transaction,
            )?;
            entries(&manifest, &index.files, dest.unwrap_or(root))?;
            index
        }
    };
    let digest = digest(&index.files);
    Ok(Captured {
        manifest,
        text,
        index,
        digest,
    })
}
pub(crate) fn write_receipt(
    root: &Path,
    captured: &Captured,
    transaction: &mut crate::transaction::Transaction,
) -> Result<(), PluginError> {
    let receipt = PackageReceipt {
        schema_version: 1,
        manifest_json: captured.text.clone(),
        digest: captured.digest.clone(),
        files: captured.index.files.clone(),
        contains_code: contains_code(&captured.manifest),
    };
    let bytes = manifest::record_bytes(&receipt)?;
    transaction.intent(
        policy::RECEIPT,
        crate::transaction::CaptureKind::File,
        CONTROL_LIMIT,
    )?;
    let mut output = policy::create_new(&root.join(policy::RECEIPT))?;
    output.write_all(&bytes).map_err(io_error)?;
    output.sync_all().map_err(io_error)
}
pub(crate) fn verify_receipt(root: &Path, id: &str) -> Result<PackageReceipt, PluginError> {
    // Receipt embeds the original manifest; allow a bounded index in addition to manifest bytes.
    let text = manifest::bounded_text(&root.join(policy::RECEIPT), CONTROL_LIMIT)?;
    let receipt: PackageReceipt = manifest::strict_record(&text)?;
    if receipt.schema_version != 1
        || !valid_index(&receipt.files)
        || digest(&receipt.files) != receipt.digest
    {
        return Err(manifest::record_error());
    }
    let m = manifest::intrinsic(&receipt.manifest_json)?;
    if m.id() != id || receipt.contains_code != contains_code(&m) {
        return Err(manifest::record_error());
    }
    let actual = scan(root, ScanMode::Payload, None, None, None)?;
    if actual.files != receipt.files
        || manifest::bounded_text(&root.join("manifest.json"), manifest::MANIFEST_LIMIT)?
            != receipt.manifest_json
    {
        return Err(manifest::record_error());
    }
    entries(&m, &receipt.files, root)?;
    Ok(receipt)
}
