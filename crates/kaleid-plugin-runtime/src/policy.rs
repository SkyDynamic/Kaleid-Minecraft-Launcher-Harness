//! Host-owned path and coordination policy. This is not an OS sandbox.
use crate::io_error;
use kaleid_api::{
    validate_relative_entry, BackendRuntime, PluginError, PluginErrorCode, PluginManifest,
    PluginPlatform,
};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    path::{Component, Path, PathBuf},
};

pub(crate) const RECEIPT: &str = ".kaleid-receipt.json";
pub(crate) const OWNER: &str = ".kaleid-owner.json";
pub(crate) const JOURNAL: &str = "journal.json";
pub(crate) const JOURNAL_NEXT: &str = "journal.next.json";
pub(crate) const TXN_PREFIX: &str = ".kaleid-txn-";
#[cfg(any(unix, test))]
pub(crate) fn portable_unix_mode(mode: u32) -> u32 {
    mode & 0o777
}
#[cfg(unix)]
pub(crate) fn file_mode(meta: &Metadata, sanitize: bool) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(if sanitize {
        portable_unix_mode(meta.mode())
    } else {
        meta.mode() & 0o7777
    })
}
#[cfg(not(unix))]
pub(crate) fn file_mode(_meta: &Metadata, _sanitize: bool) -> Option<u32> {
    None
}
#[cfg(test)]
mod mode_tests {
    #[test]
    fn unix_payload_mode_preserves_rwx_and_strips_every_special_bit() {
        for (input, want) in [
            (0o104755, 0o755),
            (0o106640, 0o640),
            (0o107777, 0o777),
            (0o100000, 0),
        ] {
            assert_eq!(super::portable_unix_mode(input), want);
        }
    }
}

pub(crate) fn entry_error(message: impl Into<String>) -> PluginError {
    PluginError::new(PluginErrorCode::EntryInvalid, message)
}
pub(crate) fn validate_id(id: &str) -> Result<(), PluginError> {
    if id.is_empty()
        || id.len() > 48
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(entry_error("Invalid plugin folder id").with_plugin_id(id));
    }
    validate_relative_entry(id).map_err(|e| e.with_plugin_id(id))
}
pub(crate) fn absolute(path: &Path) -> Result<PathBuf, PluginError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(io_error)?.join(path)
    };
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(entry_error("Host path must not contain parent traversal"));
    }
    Ok(path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect())
}
pub(crate) fn is_link(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}
pub(crate) fn check_path(path: &Path) -> Result<(), PluginError> {
    let path = absolute(path)?;
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if is_link(&meta) => {
                return Err(entry_error(
                    "Symlink or reparse point is forbidden in package/control paths",
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    Ok(())
}
pub(crate) fn exists(path: &Path) -> Result<bool, PluginError> {
    check_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(e)),
    }
}
pub(crate) fn ensure_dir(path: &Path) -> Result<(), PluginError> {
    check_path(path)?;
    fs::create_dir_all(path).map_err(io_error)?;
    check_path(path)?;
    if !fs::symlink_metadata(path).map_err(io_error)?.is_dir() {
        return Err(entry_error("Expected host-owned directory"));
    }
    Ok(())
}
pub(crate) fn open_read(path: &Path) -> Result<File, PluginError> {
    check_path(path)?;
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if is_link(&meta) || !meta.is_file() {
        return Err(entry_error("Package input must be a regular file"));
    }
    Ok(file)
}
pub(crate) fn create_new(path: &Path) -> Result<File, PluginError> {
    check_path(path)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    if is_link(&file.metadata().map_err(io_error)?) {
        return Err(entry_error("Control output is a reparse point"));
    }
    Ok(file)
}
pub(crate) fn lock_file(path: &Path) -> Result<File, PluginError> {
    check_path(path)?;
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if is_link(&meta) || !meta.is_file() {
        return Err(entry_error("Coordination lock must be a regular file"));
    }
    Ok(file)
}
pub(crate) fn reserved(relative: &str) -> bool {
    relative.split('/').any(|part| {
        let p = part.to_ascii_lowercase();
        matches!(
            p.as_str(),
            RECEIPT
                | OWNER
                | JOURNAL
                | JOURNAL_NEXT
                | ".plugin-install.lock"
                | ".plugin-runtime-locks"
        ) || p.starts_with(TXN_PREFIX)
    })
}
pub(crate) fn platform(name: &str) -> Result<PluginPlatform, PluginError> {
    match name {
        "windows-x86_64" => Ok(PluginPlatform::WindowsX86_64),
        "windows-x86" => Ok(PluginPlatform::WindowsX86),
        "windows-aarch64" => Ok(PluginPlatform::WindowsAarch64),
        "linux-x86_64" => Ok(PluginPlatform::LinuxX86_64),
        "linux-aarch64" => Ok(PluginPlatform::LinuxAarch64),
        "macos-x86_64" => Ok(PluginPlatform::MacosX86_64),
        "macos-aarch64" => Ok(PluginPlatform::MacosAarch64),
        _ => Err(PluginError::new(
            PluginErrorCode::UnsupportedPlatform,
            "Unknown current plugin platform",
        )),
    }
}
pub(crate) fn compatible(
    manifest: &PluginManifest,
    app: &str,
    current: &str,
) -> Result<(), PluginError> {
    let current_platform = platform(current)?;
    kaleid_api::validate_manifest(manifest, app)?;
    if let PluginManifest::V2(m) = manifest {
        if !m.platforms.is_empty() && !m.platforms.contains(&current_platform) {
            return Err(PluginError::new(
                PluginErrorCode::UnsupportedPlatform,
                "Plugin does not support current platform",
            )
            .with_plugin_id(&m.id));
        }
        if current.starts_with("windows-") {
            if let Some(backend) = &m.entrypoints.backend {
                let entry = backend.entry.to_ascii_lowercase();
                if backend.runtime == BackendRuntime::Executable
                    && (entry.ends_with(".bat") || entry.ends_with(".cmd"))
                {
                    return Err(entry_error(
                        "Windows executable entry cannot be an implicit shell .bat/.cmd program",
                    )
                    .with_plugin_id(&m.id));
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn signature(manifest: &PluginManifest) -> Option<&str> {
    match manifest {
        PluginManifest::V1(m) => m.signature.as_deref(),
        PluginManifest::V2(m) => m.signature.as_deref(),
    }
}
pub(crate) fn trust(manifest: &PluginManifest, allow: bool) -> Result<(), PluginError> {
    if signature(manifest).is_some() {
        return Err(PluginError::new(
            PluginErrorCode::SignatureUnsupported,
            "signature is unsupported; signed verification is not implemented",
        )
        .with_plugin_id(manifest.id()));
    }
    if !allow {
        return Err(PluginError::new(
            PluginErrorCode::UnsignedNotAllowed,
            "Unsigned package requires explicit allow_unsigned consent",
        )
        .with_plugin_id(manifest.id()));
    }
    Ok(())
}
