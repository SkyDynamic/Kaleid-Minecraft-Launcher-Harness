use std::collections::HashSet;

use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Value;

use super::{
    CommandContribution, ManifestStep, ManifestV2, PluginError, PluginErrorCode, PluginManifest,
    UiMode, MAX_MANIFEST_BYTES,
};

/// Decode the API-specific shape, without intrinsic validation or APP compatibility checks.
/// This is the inventory boundary: an incompatible installed plugin must still be inspectable.
/// The original text is decoded a second time so duplicate known fields are not lost in a Value map.
pub fn decode_manifest(text: &str) -> Result<PluginManifest, PluginError> {
    if text.len() > MAX_MANIFEST_BYTES {
        return Err(PluginError::new(
            PluginErrorCode::PackageTooLarge,
            "Manifest exceeds the 256 KiB UTF-8 byte limit",
        ));
    }

    #[derive(Deserialize)]
    struct Header {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        api: Option<Value>,
        #[serde(flatten)]
        fields: serde_json::Map<String, Value>,
    }

    let header: Header = serde_json::from_str(text).map_err(|_| {
        PluginError::new(
            PluginErrorCode::InvalidManifest,
            "Malformed manifest JSON object",
        )
    })?;
    let mut error = PluginError::new(
        PluginErrorCode::InvalidManifest,
        "Manifest must contain a string api field and the exact API-specific structure",
    );
    error.plugin_id = header.id;
    let api = header.api.as_ref().and_then(Value::as_str);
    if matches!(api, Some("1" | "2")) && !known_json_objects(&header.fields) {
        return Err(error);
    }
    match api {
        Some("1") => serde_json::from_str(text)
            .map(PluginManifest::V1)
            .map_err(|_| error),
        Some("2") => serde_json::from_str(text)
            .map(PluginManifest::V2)
            .map_err(|_| error),
        Some(_) => {
            error.code = PluginErrorCode::UnsupportedApi;
            error.message = "Only manifest API 1 and API 2 are supported".into();
            Err(error)
        }
        None => Err(error),
    }
}

// Serde struct derives accept positional sequences too. Check only the known object
// boundaries; patch/config/value/schema payloads are deliberately not traversed.
fn known_json_objects(fields: &serde_json::Map<String, Value>) -> bool {
    for field in ["entrypoints", "contributes", "permissions"] {
        if fields.get(field).is_some_and(|value| !value.is_object()) {
            return false;
        }
    }
    if let Some(backend) = fields
        .get("entrypoints")
        .and_then(|value| value.get("backend"))
    {
        if !backend.is_null() && !backend.is_object() {
            return false;
        }
        if backend
            .get("runtime")
            .is_some_and(|value| !value.is_string())
        {
            return false;
        }
    }
    // Unit-enum derives also accept externally tagged {"variant":null} objects.
    // Only canonical string wire values are valid at the strict manifest ingress.
    if fields.get("platforms").is_some_and(|value| {
        !value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    }) {
        return false;
    }
    if !object_items(fields.get("conflicts")) || !object_items(fields.get("steps")) {
        return false;
    }
    if let Some(contributes) = fields.get("contributes") {
        for field in ["ui", "commands", "services"] {
            if !object_items(contributes.get(field)) {
                return false;
            }
        }
        if contributes
            .get("ui")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|ui| ui.get("mode").is_some_and(|value| !value.is_string()))
            })
        {
            return false;
        }
        if let Some(services) = contributes.get("services").and_then(Value::as_array) {
            if services
                .iter()
                .any(|service| !object_items(service.get("methods")))
            {
                return false;
            }
        }
    }
    true
}

fn object_items(value: Option<&Value>) -> bool {
    match value {
        None => true,
        Some(Value::Array(items)) => items.iter().all(Value::is_object),
        Some(_) => false,
    }
}

/// Validate all intrinsic declarations without comparing the application's version.
/// This includes syntax of appVersion, paths, schemas' outer types, and conflict declarations;
/// it does not query installed plugins, the filesystem, the OS, or host catalogs.
pub fn validate_manifest_structure(manifest: &PluginManifest) -> Result<(), PluginError> {
    let id = manifest.id();
    if !valid_id(id) {
        return Err(invalid(
            id,
            "Plugin id must be 1..=48 bytes of lowercase ASCII a-z, 0-9, or -",
        ));
    }
    nonempty(id, manifest.name(), "name")?;
    nonempty(id, manifest.version(), "version")?;
    match manifest {
        PluginManifest::V1(v1) => {
            validate_api(id, &v1.api, "1")?;
            if v1.steps.is_empty() {
                return Err(invalid(id, "API 1 requires nonempty steps"));
            }
        }
        PluginManifest::V2(v2) => {
            validate_api(id, &v2.api, "2")?;
            validate_v2(v2)?;
        }
    }
    validate_steps(id, manifest.steps())
}

/// Validate intrinsic rules first, then API 2 application compatibility using semver semantics.
/// API 1 intentionally retains its historical non-SemVer version and lack of an APP constraint.
pub fn validate_manifest(manifest: &PluginManifest, app_version: &str) -> Result<(), PluginError> {
    validate_manifest_structure(manifest)?;
    if let PluginManifest::V2(v2) = manifest {
        let requirement = required_version(v2)?;
        let version = Version::parse(app_version).map_err(|_| {
            compatibility_error(
                v2,
                app_version,
                PluginErrorCode::InvalidManifest,
                "Application version must be a valid semantic version",
            )
        })?;
        if !requirement.matches(&version) {
            return Err(compatibility_error(
                v2,
                app_version,
                PluginErrorCode::AppVersionMismatch,
                "Plugin appVersion requirement does not match the current application version",
            ));
        }
    }
    Ok(())
}

/// Strict decoding plus intrinsic and APP compatibility validation. Success does not establish trust.
pub fn parse_manifest(text: &str, app_version: &str) -> Result<PluginManifest, PluginError> {
    let manifest = decode_manifest(text)?;
    validate_manifest(&manifest, app_version)?;
    Ok(manifest)
}

fn compatibility_error(
    manifest: &ManifestV2,
    app_version: &str,
    code: PluginErrorCode,
    message: &str,
) -> PluginError {
    let mut error = PluginError::new(code, message).with_plugin_id(&manifest.id);
    error.app_version = Some(app_version.into());
    error.required_range = Some(manifest.app_version.clone());
    error
}

fn required_version(manifest: &ManifestV2) -> Result<VersionReq, PluginError> {
    if !manifest.app_version.trim().is_empty() {
        if let Ok(requirement) = VersionReq::parse(&manifest.app_version) {
            return Ok(requirement);
        }
    }
    let mut error = invalid(
        &manifest.id,
        "appVersion must be a nonempty valid semver requirement",
    );
    error.required_range = Some(manifest.app_version.clone());
    Err(error)
}

fn invalid(id: &str, message: impl Into<String>) -> PluginError {
    PluginError::new(PluginErrorCode::InvalidManifest, message).with_plugin_id(id)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn nonempty(id: &str, value: &str, field: &str) -> Result<(), PluginError> {
    if value.trim().is_empty() {
        Err(invalid(id, format!("{field} must be nonempty")))
    } else {
        Ok(())
    }
}

fn validate_api(id: &str, actual: &str, expected: &str) -> Result<(), PluginError> {
    if actual != expected {
        Err(PluginError::new(
            PluginErrorCode::UnsupportedApi,
            "Manifest api must match its API-specific model",
        )
        .with_plugin_id(id))
    } else {
        Ok(())
    }
}

fn validate_v2(manifest: &ManifestV2) -> Result<(), PluginError> {
    let id = &manifest.id;
    if Version::parse(&manifest.version).is_err() {
        return Err(invalid(
            id,
            "API 2 version must be a valid semantic version",
        ));
    }
    required_version(manifest)?;
    validate_conflicts_and_replacements(manifest)?;
    let mut platforms = HashSet::new();
    if manifest
        .platforms
        .iter()
        .any(|platform| !platforms.insert(platform))
    {
        return Err(invalid(id, "platforms must not contain duplicates"));
    }

    let entrypoints = &manifest.entrypoints;
    if let Some(frontend) = &entrypoints.frontend {
        validate_relative_entry(frontend).map_err(|error| error.with_plugin_id(id))?;
        if !frontend.ends_with(".html") {
            return Err(PluginError::new(
                PluginErrorCode::EntryInvalid,
                "Frontend entry must have a .html extension",
            )
            .with_plugin_id(id));
        }
    }
    if let Some(backend) = &entrypoints.backend {
        validate_relative_entry(&backend.entry).map_err(|error| error.with_plugin_id(id))?;
    }
    if entrypoints.frontend.is_none() && entrypoints.backend.is_none() && manifest.steps.is_empty()
    {
        return Err(invalid(
            id,
            "API 2 requires a frontend, backend, or nonempty steps",
        ));
    }
    let contributes = &manifest.contributes;
    if !contributes.ui.is_empty() && entrypoints.frontend.is_none() {
        return Err(invalid(id, "UI contributions require a frontend entry"));
    }
    if (!contributes.commands.is_empty()
        || !contributes.services.is_empty()
        || !manifest.replaces.is_empty())
        && entrypoints.backend.is_none()
    {
        return Err(invalid(
            id,
            "Commands, services, and replaces require a backend entry",
        ));
    }
    for ui in &contributes.ui {
        nonempty(id, &ui.target, "UI target")?;
        if ui.target == "app.root" && ui.mode != UiMode::Replace {
            return Err(invalid(
                id,
                "app.root supports only replace UI contributions",
            ));
        }
    }
    validate_commands(id, &contributes.commands)?;
    let mut services = HashSet::new();
    for service in &contributes.services {
        validate_segment(id, &service.name)?;
        if !services.insert(&service.name) {
            return Err(invalid(id, "Service names must be unique"));
        }
        if service.contract_version == 0 || service.methods.is_empty() {
            return Err(invalid(
                id,
                "Services require a positive contractVersion and nonempty methods",
            ));
        }
        validate_commands(id, &service.methods)?;
    }
    validate_permissions(id, &manifest.permissions.host_commands)?;
    validate_permissions(id, &manifest.permissions.events)
}

fn validate_conflicts_and_replacements(manifest: &ManifestV2) -> Result<(), PluginError> {
    let id = &manifest.id;
    let mut conflicts = HashSet::new();
    for conflict in &manifest.conflicts {
        if !valid_id(&conflict.id) || conflict.id == *id || !conflicts.insert(conflict.id.as_str())
        {
            return Err(invalid(
                id,
                "Conflict ids must be valid, unique, and not the declaring plugin",
            ));
        }
        nonempty(id, &conflict.reason, "Conflict reason")?;
        if conflict.reason.chars().count() > 512 {
            return Err(invalid(
                id,
                "Conflict reason must not exceed 512 Unicode scalar characters",
            ));
        }
    }
    const PROTECTED: &[&str] = &[
        "kernel",
        "plugin-manager",
        "launcher-logger",
        "process-broker",
        "app-identity",
        "native-permissions",
        "recovery",
    ];
    let mut replaces = HashSet::new();
    for target in &manifest.replaces {
        if !valid_id(target) || target == id || !replaces.insert(target) {
            return Err(invalid(
                id,
                "replaces ids must be valid, unique, and not the declaring plugin",
            ));
        }
        if conflicts.contains(target.as_str()) {
            return Err(invalid(
                id,
                "An id must not appear in both conflicts and replaces",
            ));
        }
        if PROTECTED.contains(&target.as_str()) {
            return Err(invalid(
                id,
                "replaces must not target protected host plugins",
            ));
        }
    }
    Ok(())
}

fn validate_segment(id: &str, name: &str) -> Result<(), PluginError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        Err(invalid(
            id,
            "Command, service, and method names must be ASCII a-z, A-Z, 0-9, _, or - segments",
        ))
    } else {
        Ok(())
    }
}

fn validate_commands(id: &str, commands: &[CommandContribution]) -> Result<(), PluginError> {
    let mut names = HashSet::new();
    for command in commands {
        validate_segment(id, &command.name)?;
        if !names.insert(&command.name) {
            return Err(invalid(
                id,
                "Command or method names must be unique within their group",
            ));
        }
        for schema in [&command.input_schema, &command.output_schema] {
            if !schema.is_object() && !schema.is_boolean() {
                return Err(invalid(
                    id,
                    "inputSchema and outputSchema must be JSON objects or booleans",
                ));
            }
        }
    }
    Ok(())
}

fn validate_permissions(id: &str, names: &[String]) -> Result<(), PluginError> {
    let mut seen = HashSet::new();
    for name in names {
        if name.trim().is_empty() || name.trim() != name || name.contains('*') || !seen.insert(name)
        {
            return Err(invalid(
                id,
                "Permission names must be exact, trimmed, nonempty, unique, and contain no *",
            ));
        }
    }
    Ok(())
}

fn validate_steps(id: &str, steps: &[ManifestStep]) -> Result<(), PluginError> {
    for (index, step) in steps.iter().enumerate() {
        let field = |name: &str| format!("steps[{}] {name}", index + 1);
        match step {
            ManifestStep::UpdateConfig { target, patch } => {
                nonempty(id, target, &field("target"))?;
                if !patch.is_object() {
                    return Err(invalid(id, field("update-config patch must be an object")));
                }
            }
            ManifestStep::DisablePlugin { target } | ManifestStep::EnablePlugin { target } => {
                nonempty(id, target, &field("target"))?;
            }
            ManifestStep::ProvideSource {
                id: source_id,
                label,
                manifest_url,
                base_url,
                ..
            } => {
                if !valid_id(source_id) {
                    return Err(invalid(id, field("provide-source id must be valid")));
                }
                nonempty(id, label, &field("label"))?;
                if !manifest_url.starts_with("https://") || !base_url.starts_with("https://") {
                    return Err(invalid(
                        id,
                        field("provide-source URLs must start with https://"),
                    ));
                }
            }
            ManifestStep::BindSlot { slot, title, view } => {
                nonempty(id, slot, &field("slot"))?;
                nonempty(id, title, &field("title"))?;
                if !view.starts_with("readonly-text:") {
                    return Err(invalid(
                        id,
                        field("bind-slot view must start with readonly-text:"),
                    ));
                }
            }
            ManifestStep::SetDefaultSetting { key, .. } => {
                nonempty(id, key, &field("key"))?;
            }
        }
    }
    Ok(())
}

/// Validate canonical package-relative path syntax only, independent of the host OS.
/// Does not resolve symlinks/reparse points, access a filesystem, or provide TOCTOU protection.
pub fn validate_relative_entry(entry: &str) -> Result<(), PluginError> {
    let error = || {
        PluginError::new(
            PluginErrorCode::EntryInvalid,
            "Entry must be a canonical package-relative /-separated path without unsafe segments",
        )
    };
    if entry.is_empty()
        || entry.starts_with('/')
        || entry.contains(['\\', ':', '%', '<', '>', '"', '|', '?', '*'])
        || entry.chars().any(char::is_control)
    {
        return Err(error());
    }
    for segment in entry.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.ends_with(['.', ' '])
        {
            return Err(error());
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        let device_suffix = stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"));
        let numbered_device = device_suffix.is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered_device {
            return Err(error());
        }
    }
    Ok(())
}
