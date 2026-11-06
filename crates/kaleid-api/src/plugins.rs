//! Public plugin manifest contract and pure validation.
//!
//! Decoding does not establish compatibility, trust, installation safety, or activation.
//! Inventory should decode and validate intrinsic declarations separately from APP compatibility.
//! Filesystem/reparse checks and runtime catalog authorization belong to later layers.

mod error;
mod manifest;
mod validation;

pub use error::{ConflictDetail, PluginError, PluginErrorCode};
pub use manifest::{
    BackendEntry, BackendRuntime, CommandContribution, ConflictDeclaration, ManifestStep,
    ManifestV1, ManifestV2, PluginContributions, PluginEntrypoints, PluginManifest,
    PluginPermissions, PluginPlatform, ServiceContribution, UiContribution, UiMode,
};
pub use validation::{
    decode_manifest, parse_manifest, validate_manifest, validate_manifest_structure,
    validate_relative_entry,
};

/// Semantic application version used by manifest API 2 (not the launcher version code).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(super) const MAX_MANIFEST_BYTES: usize = 256 * 1024;
