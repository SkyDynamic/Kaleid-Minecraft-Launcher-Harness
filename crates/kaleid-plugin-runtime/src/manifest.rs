//! The one manifest ingress is kaleid_api's strict decoder plus intrinsic validation.
use crate::{io_error, policy};
use kaleid_api::{
    decode_manifest, validate_manifest_structure, PluginError, PluginErrorCode, PluginManifest,
};
use serde::de::DeserializeOwned;
use std::{io::Read, path::Path};
pub(crate) const MANIFEST_LIMIT: u64 = 256 * 1024;
pub(crate) fn bounded_text(path: &Path, limit: u64) -> Result<String, PluginError> {
    bounded_text_with_mode(path, limit).map(|(text, _)| text)
}
pub(crate) fn bounded_text_with_mode(
    path: &Path,
    limit: u64,
) -> Result<(String, Option<u32>), PluginError> {
    let file = policy::open_read(path)?;
    let unix_mode = policy::file_mode(&file.metadata().map_err(io_error)?, true);
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(PluginError::new(
            PluginErrorCode::PackageTooLarge,
            "Manifest/control record exceeds byte bound",
        ));
    }
    String::from_utf8(bytes)
        .map(|text| (text, unix_mode))
        .map_err(|_| {
            PluginError::new(
                PluginErrorCode::InvalidManifest,
                "Manifest/control text must be UTF-8",
            )
        })
}
pub(crate) fn intrinsic(text: &str) -> Result<PluginManifest, PluginError> {
    let m = decode_manifest(text)?;
    validate_manifest_structure(&m)?;
    Ok(m)
}
/// Original-text typed decode keeps duplicate field rejection; Value only gates object shapes.
pub(crate) fn strict_record<T: DeserializeOwned>(text: &str) -> Result<T, PluginError> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| record_error())?;
    if !value.is_object() {
        return Err(record_error());
    }
    for field in ["phase", "operation"] {
        if value.get(field).is_some_and(|v| !v.is_string()) {
            return Err(record_error());
        }
    }
    if let Some(items) = value
        .get("captureIntents")
        .and_then(serde_json::Value::as_array)
    {
        if items
            .iter()
            .any(|i| i.get("kind").is_some_and(|v| !v.is_string()))
        {
            return Err(record_error());
        }
    }
    for field in ["files", "backupFiles", "candidateFiles", "captureIntents"] {
        if let Some(array) = value.get(field) {
            if !array
                .as_array()
                .is_some_and(|items| items.iter().all(serde_json::Value::is_object))
            {
                return Err(record_error());
            }
        }
    }
    serde_json::from_str(text).map_err(|_| record_error())
}
pub(crate) fn record_error() -> PluginError {
    PluginError::new(
        PluginErrorCode::InventoryInvalid,
        "Invalid or tampered installer record; manual repair required",
    )
}
struct LimitedJson {
    bytes: Vec<u8>,
    count: usize,
    collect: bool,
    exceeded: bool,
}
impl std::io::Write for LimitedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.count.saturating_add(bytes.len()) as u64 > crate::CONTROL_RECORD_MAX_BYTES {
            self.exceeded = true;
            return Err(std::io::Error::other("control record capacity exceeded"));
        }
        self.count += bytes.len();
        if self.collect {
            self.bytes.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn capacity_error() -> PluginError {
    PluginError::new(
        PluginErrorCode::PackageTooLarge,
        "Installer control record exceeds separate 16 MiB metadata capacity",
    )
}
fn encode(value: &impl serde::Serialize, collect: bool) -> Result<LimitedJson, PluginError> {
    let mut output = LimitedJson {
        bytes: vec![],
        count: 0,
        collect,
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut output, value) {
        return Err(if output.exceeded {
            capacity_error()
        } else {
            io_error(error)
        });
    }
    Ok(output)
}
pub(crate) fn record_bytes(value: &impl serde::Serialize) -> Result<Vec<u8>, PluginError> {
    Ok(encode(value, true)?.bytes)
}
pub(crate) fn charge_index(
    budget: &mut usize,
    value: &impl serde::Serialize,
) -> Result<(), PluginError> {
    let count = encode(value, false)?.count + 1;
    *budget = budget.saturating_add(count);
    if *budget as u64 > crate::CONTROL_RECORD_MAX_BYTES {
        return Err(capacity_error());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_control_serialization_is_bounded_before_any_file_write() {
        let text = "x".repeat(16 * 1024 * 1024);
        let result = record_bytes(&serde_json::json!({"record":text}));
        assert!(
            matches!(
                result,
                Err(PluginError {
                    code: PluginErrorCode::PackageTooLarge,
                    ..
                })
            ),
            "oversized generated JSON was accepted"
        );
    }
}
