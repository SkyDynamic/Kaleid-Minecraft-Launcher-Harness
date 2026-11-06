//! Real filesystem tests; no packaged entrypoint is executed.
use kaleid_api::{PluginErrorCode, PluginManifest};
use kaleid_plugin_runtime::{
    CaptureIntent, CaptureKind, InventoryState, JournalOperation, JournalPhase, PackageFile,
    PackageManager, TransactionJournal, TransactionOwner,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};
use tempfile::TempDir;

fn manager(root: &Path) -> PackageManager {
    PackageManager::new(
        root.to_path_buf(),
        vec!["news".into(), "kernel".into()],
        "0.1.0".into(),
        "windows-x86_64".into(),
    )
}
fn v1(id: &str) -> Value {
    json!({"id":id,"name":"Panel","version":"legacy release","api":"1",
        "steps":[{"op":"bind-slot","slot":"overview.notices","title":"Notice","view":"readonly-text:Hi"}]})
}
fn v2(id: &str) -> Value {
    json!({"id":id,"name":"Package","version":"1.0.0","api":"2","appVersion":">=0.1.0, <0.2.0",
        "conflicts":[],"entrypoints":{"frontend":"ui/index.html","backend":{"runtime":"node","entry":"backend.js"}}})
}
fn package(value: &Value) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("ui")).unwrap();
    fs::write(dir.path().join("manifest.json"), value.to_string()).unwrap();
    fs::write(
        dir.path().join("ui/index.html"),
        b"<!doctype html><p>Hello</p>",
    )
    .unwrap();
    fs::write(
        dir.path().join("backend.js"),
        b"require('fs').writeFileSync('EXECUTED', 'bad');",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in ["manifest.json", "backend.js", "ui/index.html"] {
            fs::set_permissions(dir.path().join(path), fs::Permissions::from_mode(0o644)).unwrap();
        }
    }
    dir
}
fn bytes(root: &Path, id: &str) -> Vec<u8> {
    fs::read(root.join("plugins").join(id).join("manifest.json")).unwrap()
}
#[test]
fn explicit_api2_custom_manifest_filename_is_canonicalized_with_all_assets() {
    let root = tempfile::tempdir().unwrap();
    let source = package(&v2("code"));
    let selected = source.path().join("plugin definition.json");
    fs::rename(source.path().join("manifest.json"), &selected).unwrap();
    let pm = manager(root.path());
    let preview = pm.preview(&selected).unwrap();
    assert!(preview
        .files
        .iter()
        .any(|f| f.relative_path == "manifest.json"));
    assert!(!preview
        .files
        .iter()
        .any(|f| f.relative_path == "plugin definition.json"));
    assert!(
        pm.preview(source.path()).is_err(),
        "directory mode must still require manifest.json"
    );
    let installed = pm.install(&selected, true).unwrap();
    assert_eq!(installed.digest, preview.digest);
    assert_eq!(
        bytes(root.path(), "code"),
        v2("code").to_string().as_bytes()
    );
    assert_eq!(
        fs::read(installed.installed_path.join("backend.js")).unwrap(),
        b"require('fs').writeFileSync('EXECUTED', 'bad');"
    );
    assert!(!installed
        .installed_path
        .join("plugin definition.json")
        .exists());
    assert!(
        pm.list()
            .unwrap()
            .iter()
            .find(|i| i.id == "code")
            .unwrap()
            .receipt_verified
    );
}
#[test]
fn explicit_api2_custom_manifest_rejects_different_canonical_root_manifest_collision() {
    let root = tempfile::tempdir().unwrap();
    let source = package(&v2("code"));
    let selected = source.path().join("selected.json");
    fs::write(&selected, v2("other").to_string()).unwrap();
    let pm = manager(root.path());
    assert_eq!(
        pm.preview(&selected).unwrap_err().code,
        PluginErrorCode::EntryInvalid
    );
    assert_eq!(
        pm.install(&selected, true).unwrap_err().code,
        PluginErrorCode::EntryInvalid
    );
    assert!(!root.path().join("plugins/other").exists());
    assert!(!root.path().join("plugins/code").exists());
    assert_eq!(
        fs::read(source.path().join("manifest.json")).unwrap(),
        v2("code").to_string().as_bytes()
    );
}

#[test]
fn v1_installs_only_captured_manifest_and_remove_keeps_private_data() {
    // Copying a v1 parent tree or uninstalling plugin-data would corrupt unrelated/user data.
    let root = tempfile::tempdir().unwrap();
    let source = package(&v1("panel"));
    let pm = manager(root.path());
    let preview = pm.preview(&source.path().join("manifest.json")).unwrap();
    assert!(!root.path().join("plugins").exists());
    assert_eq!(preview.files.len(), 1);
    assert_eq!(preview.files[0].relative_path, "manifest.json");
    assert!(!preview.contains_code);
    assert!(preview.requires_unsigned_consent);
    assert_eq!(
        pm.install(source.path(), false).unwrap_err().code,
        PluginErrorCode::UnsignedNotAllowed
    );
    let installed = pm
        .install(&source.path().join("manifest.json"), true)
        .unwrap();
    assert!(matches!(installed.manifest, PluginManifest::V1(_)));
    assert_eq!(installed.installed_path, root.path().join("plugins/panel"));
    assert_eq!(
        bytes(root.path(), "panel"),
        v1("panel").to_string().as_bytes()
    );
    assert!(!installed.installed_path.join("backend.js").exists());
    fs::create_dir_all(root.path().join("plugin-data/panel")).unwrap();
    fs::write(root.path().join("plugin-data/panel/state"), b"user data").unwrap();
    pm.remove("panel").unwrap();
    assert_eq!(
        fs::read(root.path().join("plugin-data/panel/state")).unwrap(),
        b"user data"
    );
    assert!(!installed.installed_path.exists());
}
#[test]
fn api1_guard_rejects_real_source_changed_to_api2_after_advisory_preview() {
    let root = tempfile::tempdir().unwrap();
    let source = package(&v1("code"));
    let pm = manager(root.path());
    assert!(matches!(
        pm.preview(source.path()).unwrap().manifest,
        PluginManifest::V1(_)
    ));
    fs::write(source.path().join("manifest.json"), v2("code").to_string()).unwrap();
    assert_eq!(
        pm.install_api1(source.path(), true).unwrap_err().code,
        PluginErrorCode::UnsupportedApi
    );
    assert!(!root.path().join("plugins/code").exists());
}

#[test]
fn signature_presence_is_never_treated_as_verified_or_unsigned() {
    let root = tempfile::tempdir().unwrap();
    for signature in [json!(""), json!("opaque")] {
        let mut value = v1("panel");
        value["signature"] = signature;
        let source = package(&value);
        for allow in [false, true] {
            assert_eq!(
                manager(root.path())
                    .install(source.path(), allow)
                    .unwrap_err()
                    .code,
                PluginErrorCode::SignatureUnsupported
            );
        }
        assert!(!root.path().join("plugins/panel").exists());
    }
    let mut value = v1("panel");
    value["signature"] = Value::Null;
    let source = package(&value);
    manager(root.path()).install(source.path(), true).unwrap();
}
#[test]
fn v2_copies_assets_indexes_literal_hashes_and_never_executes_code() {
    let root = tempfile::tempdir().unwrap();
    let source = package(&v2("code"));
    let pm = manager(root.path());
    assert_eq!(
        pm.preview(source.path()).unwrap().digest,
        if cfg!(unix) {
            "4cc5746aec24d47088f3090d67b301e54b1a44ac59cee2e6df5f5b896c9cfd88"
        } else {
            "979baee476ec4112c402d9f9c71673022fa4940d9e0f21511c4d3d156e1ed22c"
        }
    );
    fs::create_dir(source.path().join("页面")).unwrap();
    fs::write(source.path().join("页面/说明.txt"), "Unicode asset").unwrap();
    fs::write(source.path().join("a.txt"), b"abc").unwrap();
    let first = pm.preview(source.path()).unwrap();
    assert!(first.contains_code);
    let a = first
        .files
        .iter()
        .find(|f| f.relative_path == "a.txt")
        .unwrap();
    assert_eq!(a.bytes, 3);
    assert_eq!(
        a.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    fs::write(source.path().join("a.txt"), b"abcd").unwrap();
    let changed = pm.preview(source.path()).unwrap();
    assert_ne!(first.digest, changed.digest);
    let installed = pm
        .install(&source.path().join("manifest.json"), true)
        .unwrap();
    assert_eq!(changed.digest, installed.digest);
    assert_eq!(
        fs::read(installed.installed_path.join("a.txt")).unwrap(),
        b"abcd"
    );
    assert_eq!(
        fs::read(installed.installed_path.join("页面/说明.txt")).unwrap(),
        b"Unicode asset"
    );
    assert!(!installed.installed_path.join("EXECUTED").exists());
    assert_eq!(
        fs::read(installed.installed_path.join("ui/index.html")).unwrap(),
        b"<!doctype html><p>Hello</p>"
    );
    assert!(!source.path().join("EXECUTED").exists());
    assert!(!root.path().join("EXECUTED").exists());
    let listed = pm.list().unwrap();
    let item = listed.iter().find(|i| i.id == "code").unwrap();
    assert!(item.receipt_verified);
    assert_eq!(item.digest.as_deref(), Some(installed.digest.as_str()));
    assert_eq!(item.files, installed.files);
}
#[test]
fn compatibility_and_actual_entry_requirements_are_enforced_without_publication() {
    let root = tempfile::tempdir().unwrap();
    let mut value = v2("code");
    value["appVersion"] = json!(">=0.2.0");
    let source = package(&value);
    assert_eq!(
        manager(root.path())
            .preview(source.path())
            .unwrap_err()
            .code,
        PluginErrorCode::AppVersionMismatch
    );
    value["appVersion"] = json!("*");
    value["platforms"] = json!(["linux-x86_64"]);
    fs::write(source.path().join("manifest.json"), value.to_string()).unwrap();
    assert_eq!(
        manager(root.path())
            .install(source.path(), true)
            .unwrap_err()
            .code,
        PluginErrorCode::UnsupportedPlatform
    );
    let unknown = PackageManager::new(root.path().into(), vec![], "0.1.0".into(), "unknown".into());
    assert_eq!(
        unknown.preview(source.path()).unwrap_err().code,
        PluginErrorCode::UnsupportedPlatform
    );
    value["platforms"] = json!([]);
    fs::write(source.path().join("manifest.json"), value.to_string()).unwrap();
    fs::remove_file(source.path().join("ui/index.html")).unwrap();
    assert_eq!(
        manager(root.path())
            .install(source.path(), true)
            .unwrap_err()
            .code,
        PluginErrorCode::EntryInvalid
    );
    assert!(!root.path().join("plugins/code").exists());
}
#[test]
fn windows_executable_batch_entrypoints_and_installer_reserved_files_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    for entry in ["run.bat", "RUN.CMD"] {
        let mut value = v2("code");
        value["entrypoints"]["backend"] = json!({"runtime":"executable","entry":entry});
        let source = package(&value);
        fs::write(source.path().join(entry), b"echo bad").unwrap();
        assert_eq!(
            manager(root.path())
                .install(source.path(), true)
                .unwrap_err()
                .code,
            PluginErrorCode::EntryInvalid
        );
    }
    for name in [
        ".kaleid-receipt.json",
        ".kaleid-owner.json",
        "journal.json",
        ".kaleid-txn-0123456789abcdef0123456789abcdef",
    ] {
        let source = package(&v2("code"));
        fs::write(source.path().join(name), b"caller metadata").unwrap();
        assert_eq!(
            manager(root.path())
                .install(source.path(), true)
                .unwrap_err()
                .code,
            PluginErrorCode::EntryInvalid,
            "{name}"
        );
    }
    assert!(!root.path().join("plugins/code").exists());
}
#[test]
fn manifest_and_actual_stream_file_package_bounds_are_enforced() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v1("large"));
    let text = v1("large").to_string();
    fs::write(
        source.path().join("manifest.json"),
        format!("{text}{}", " ".repeat(262144 - text.len())),
    )
    .unwrap();
    pm.preview(source.path()).unwrap();
    fs::write(
        source.path().join("manifest.json"),
        format!("{{{}", "界".repeat(90000)),
    )
    .unwrap();
    assert_eq!(
        pm.preview(source.path()).unwrap_err().code,
        PluginErrorCode::PackageTooLarge
    );
    let source = package(&v2("large"));
    let file = fs::File::create(source.path().join("oversize")).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    assert_eq!(
        pm.install(source.path(), true).unwrap_err().code,
        PluginErrorCode::PackageTooLarge
    );
    fs::remove_file(source.path().join("oversize")).unwrap();
    for n in 0..4 {
        fs::File::create(source.path().join(format!("large-{n}")))
            .unwrap()
            .set_len(64 * 1024 * 1024)
            .unwrap();
    }
    assert_eq!(
        pm.install(source.path(), true).unwrap_err().code,
        PluginErrorCode::PackageTooLarge
    );
    assert!(!root.path().join("plugins/large").exists());
}
#[test]
fn inventory_and_bidirectional_conflicts_include_incompatible_disabled_and_builtins() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let mut value = v2("old");
    value["conflicts"] = json!([{"id":"new","reason":"existing owner"}]);
    let old = package(&value);
    pm.install(old.path(), true).unwrap();
    let old_bytes = bytes(root.path(), "old");
    // No activation/grant filter: direct compatibility change leaves the parsed conflicts inspectable.
    fs::remove_file(root.path().join("plugins/old/.kaleid-receipt.json")).unwrap();
    value["appVersion"] = json!(">=0.2.0");
    fs::write(
        root.path().join("plugins/old/manifest.json"),
        value.to_string(),
    )
    .unwrap();
    let incompatible_tree = identity(&root.path().join("plugins/old"));
    fs::write(
        root.path().join("config.json"),
        br#"{"plugins":{"old":{"enabled":false}}}"#,
    )
    .unwrap();
    let mut value = v2("new");
    value["conflicts"] =
        json!([{"id":"old","reason":"incoming owner"},{"id":"news","reason":"builtin owner"}]);
    let new = package(&value);
    let error = pm.install(new.path(), true).unwrap_err();
    assert_eq!(error.code, PluginErrorCode::Conflict);
    let details: Vec<_> = error
        .conflicts
        .iter()
        .map(|c| (c.id.as_str(), c.reason.as_str(), c.declared_by.as_str()))
        .collect();
    assert_eq!(
        details,
        vec![
            ("new", "existing owner", "old"),
            ("news", "builtin owner", "new"),
            ("old", "incoming owner", "new")
        ]
    );
    assert!(!root.path().join("plugins/new").exists());
    assert_ne!(bytes(root.path(), "old"), old_bytes);
    assert_eq!(
        identity(&root.path().join("plugins/old")),
        incompatible_tree
    );
    let list = pm.list().unwrap();
    assert_eq!(
        list.iter().find(|i| i.id == "old").unwrap().state,
        InventoryState::Incompatible
    );
    assert_eq!(
        fs::read(root.path().join("config.json")).unwrap(),
        br#"{"plugins":{"old":{"enabled":false}}}"#
    );
}
#[test]
fn malformed_inventory_remains_visible_blocks_mutations_but_can_be_removed() {
    for (folder, text) in [
        ("broken", "{".into()),
        ("missing", String::new()),
        ("mismatch", v1("other").to_string()),
        (
            "intrinsic",
            json!({"id":"intrinsic","name":"Bad","version":"1","api":"1","steps":[]}).to_string(),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let pm = manager(root.path());
        fs::create_dir_all(root.path().join("plugins").join(folder)).unwrap();
        if !text.is_empty() {
            fs::write(
                root.path()
                    .join("plugins")
                    .join(folder)
                    .join("manifest.json"),
                text,
            )
            .unwrap();
        }
        let items = pm.list().unwrap();
        let item = items.iter().find(|i| i.id == folder).unwrap();
        assert_eq!(item.state, InventoryState::Invalid);
        let new = package(&v1("new"));
        assert_eq!(
            pm.install(new.path(), true).unwrap_err().code,
            PluginErrorCode::InventoryInvalid
        );
        pm.remove(folder).unwrap();
        assert!(!root.path().join("plugins").join(folder).exists());
    }
}
#[test]
fn default_same_id_and_builtin_ids_never_overwrite_but_explicit_update_does() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("old"));
    pm.install(source.path(), true).unwrap();
    let before = bytes(root.path(), "old");
    let mut value = v2("old");
    value["version"] = json!("2.0.0");
    let update = package(&value);
    assert_eq!(
        pm.install(update.path(), true).unwrap_err().code,
        PluginErrorCode::AlreadyInstalled
    );
    assert_eq!(bytes(root.path(), "old"), before);
    pm.update(update.path(), true).unwrap();
    assert_eq!(bytes(root.path(), "old"), value.to_string().as_bytes());
    let builtin = package(&v1("news"));
    assert_eq!(
        pm.install(builtin.path(), true).unwrap_err().code,
        PluginErrorCode::Unauthorized
    );
    assert_eq!(
        pm.update(builtin.path(), true).unwrap_err().code,
        PluginErrorCode::Unauthorized
    );
    assert_eq!(
        pm.remove("news").unwrap_err().code,
        PluginErrorCode::Unauthorized
    );
    assert!(!root.path().join("plugins/news").exists());
}
#[test]
fn two_shared_runtime_leases_block_update_remove_until_both_are_released() {
    let root = tempfile::tempdir().unwrap();
    let first = manager(root.path());
    let second = manager(root.path());
    let source = package(&v2("code"));
    first.install(source.path(), true).unwrap();
    let before = bytes(root.path(), "code");
    fs::create_dir_all(root.path().join("plugin-data/code")).unwrap();
    fs::write(
        root.path().join("plugin-data/code/state"),
        b"shared private state",
    )
    .unwrap();
    fs::write(root.path().join("config.json"), b"unchanged config").unwrap();
    let a = first.acquire_runtime_lease("code").unwrap();
    let b = second.acquire_runtime_lease("code").unwrap();
    for pm in [&first, &second] {
        assert_eq!(
            pm.update(source.path(), true).unwrap_err().code,
            PluginErrorCode::Busy
        );
        assert_eq!(pm.remove("code").unwrap_err().code, PluginErrorCode::Busy);
    }
    assert_eq!(bytes(root.path(), "code"), before);
    assert_eq!(
        fs::read(root.path().join("plugin-data/code/state")).unwrap(),
        b"shared private state"
    );
    assert_eq!(
        fs::read(root.path().join("config.json")).unwrap(),
        b"unchanged config"
    );
    drop(a);
    assert_eq!(
        second.remove("code").unwrap_err().code,
        PluginErrorCode::Busy
    );
    drop(b);
    second.update(source.path(), true).unwrap();
    second.remove("code").unwrap();
    assert_eq!(
        fs::read(root.path().join("plugin-data/code/state")).unwrap(),
        b"shared private state"
    );
    assert_eq!(
        fs::read(root.path().join("config.json")).unwrap(),
        b"unchanged config"
    );
    assert_eq!(
        first.acquire_runtime_lease("../outside").unwrap_err().code,
        PluginErrorCode::EntryInvalid
    );
}
// Independent recovery fixture serializer/indexer, not a production failpoint.
fn identity(root: &Path) -> (Vec<PackageFile>, Vec<String>) {
    fn walk(root: &Path, dir: &Path, files: &mut Vec<PackageFile>, dirs: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            if path.is_dir() {
                dirs.push(relative);
                walk(root, &path, files, dirs);
            } else {
                let bytes = fs::read(&path).unwrap();
                #[cfg(unix)]
                let unix_mode = {
                    use std::os::unix::fs::MetadataExt;
                    Some(fs::metadata(&path).unwrap().mode() & 0o7777)
                };
                #[cfg(not(unix))]
                let unix_mode = None;
                files.push(PackageFile {
                    relative_path: relative,
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                    bytes: bytes.len() as u64,
                    unix_mode,
                });
            }
        }
    }
    let mut files = vec![];
    let mut dirs = vec![];
    walk(root, root, &mut files, &mut dirs);
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    dirs.sort();
    (files, dirs)
}
fn journal_fixture(
    root: &Path,
    operation: JournalOperation,
    phase: JournalPhase,
    old: &Path,
    new: Option<(&Path, &str)>,
) -> std::path::PathBuf {
    let name = ".kaleid-txn-0123456789abcdef0123456789abcdef";
    let txn = root.join("plugins").join(name);
    fs::create_dir(&txn).unwrap();
    let (backup_files, backup_directories) = identity(old);
    let (candidate_files, candidate_directories) =
        new.map(|(p, _)| identity(p)).unwrap_or_default();
    let capture_intents = intents_for(&candidate_files, &candidate_directories);
    let journal = TransactionJournal {
        schema_version: 1,
        transaction: name.into(),
        plugin_id: "code".into(),
        operation,
        phase,
        candidate_digest: new.map(|(_, d)| d.into()),
        backup_files,
        backup_directories,
        candidate_files,
        candidate_directories,
        capture_intents,
    };
    fs::write(
        txn.join(".kaleid-owner.json"),
        serde_json::to_vec(&TransactionOwner {
            schema_version: 1,
            transaction: name.into(),
        })
        .unwrap(),
    )
    .unwrap();
    fs::write(
        txn.join("journal.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    txn
}
#[test]
fn recovery_restores_old_moved_update_and_before_commit_remove() {
    // Losing backup restoration would erase the original after a process interruption.
    for operation in [JournalOperation::Update, JournalOperation::Remove] {
        let root = tempfile::tempdir().unwrap();
        let pm = manager(root.path());
        let source = package(&v2("code"));
        let old = pm.install(source.path(), true).unwrap();
        let expected = identity(&old.installed_path);
        let txn = journal_fixture(
            root.path(),
            operation,
            JournalPhase::OldMoved,
            &old.installed_path,
            None,
        );
        // Update needs a candidate receipt identity; use the same old package for this interrupted pre-publication fixture.
        if operation == JournalOperation::Update {
            let path = txn.join("journal.json");
            let mut j: TransactionJournal =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            j.candidate_digest = Some(old.digest.clone());
            j.candidate_files = expected.0.clone();
            j.candidate_directories = expected.1.clone();
            j.capture_intents = intents_for(&expected.0, &expected.1);
            fs::write(path, serde_json::to_vec(&j).unwrap()).unwrap();
        }
        fs::rename(&old.installed_path, txn.join("backup")).unwrap();
        pm.list().unwrap();
        assert_eq!(identity(&old.installed_path), expected);
        assert!(!txn.exists());
    }
}
#[test]
fn recovery_detects_publish_after_rename_and_verifies_before_backup_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let newroot = tempfile::tempdir().unwrap();
    let mut value = v2("code");
    value["version"] = json!("2.0.0");
    let source = package(&value);
    let new = manager(newroot.path())
        .install(source.path(), true)
        .unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Update,
        JournalPhase::OldMoved,
        &old.installed_path,
        Some((&new.installed_path, &new.digest)),
    );
    fs::rename(&old.installed_path, txn.join("backup")).unwrap();
    fs::rename(&new.installed_path, &old.installed_path).unwrap();
    pm.list().unwrap();
    assert_eq!(bytes(root.path(), "code"), value.to_string().as_bytes());
    assert!(!txn.exists());
}
#[test]
fn recovery_refuses_foreign_target_tampered_backup_and_unmarked_transaction() {
    for tamper_backup in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let pm = manager(root.path());
        let source = package(&v2("code"));
        let old = pm.install(source.path(), true).unwrap();
        let txn = journal_fixture(
            root.path(),
            JournalOperation::Update,
            JournalPhase::OldMoved,
            &old.installed_path,
            Some((&old.installed_path, &old.digest)),
        );
        fs::rename(&old.installed_path, txn.join("backup")).unwrap();
        if tamper_backup {
            fs::write(txn.join("backup/unrecorded"), b"foreign").unwrap();
        } else {
            fs::create_dir(&old.installed_path).unwrap();
            fs::write(old.installed_path.join("foreign"), b"untouched").unwrap();
        }
        assert_eq!(
            pm.list().unwrap_err().code,
            PluginErrorCode::InventoryInvalid
        );
        assert!(txn.join("backup").exists());
        if !tamper_backup {
            assert_eq!(
                fs::read(old.installed_path.join("foreign")).unwrap(),
                b"untouched"
            );
        }
    }
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(
        root.path()
            .join("plugins/.kaleid-txn-0123456789abcdef0123456789abcdef"),
    )
    .unwrap();
    assert_eq!(
        manager(root.path()).list().unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
}
#[test]
fn recovery_rejects_path_unknown_duplicate_and_array_journals_without_outside_effects() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Remove,
        JournalPhase::Prepared,
        &old.installed_path,
        None,
    );
    let path = txn.join("journal.json");
    let original = fs::read_to_string(&path).unwrap();
    let sentinel = tempfile::tempdir().unwrap();
    fs::write(sentinel.path().join("sentinel"), b"untouched").unwrap();
    for bad in [
        original.replace("\"pluginId\":\"code\"", "\"pluginId\":\"../outside\""),
        original.replace(
            "\"schemaVersion\":1",
            "\"schemaVersion\":1,\"schemaVersion\":1",
        ),
        original.replace(
            "\"schemaVersion\":1",
            "\"schemaVersion\":1,\"unknown\":true",
        ),
        "[1,\"outside\"]".into(),
    ] {
        fs::write(&path, bad).unwrap();
        assert_eq!(
            pm.list().unwrap_err().code,
            PluginErrorCode::InventoryInvalid
        );
        assert_eq!(
            bytes(root.path(), "code"),
            v2("code").to_string().as_bytes()
        );
        assert_eq!(
            fs::read(sentinel.path().join("sentinel")).unwrap(),
            b"untouched"
        );
    }
}
#[cfg(unix)]
#[test]
fn unix_executable_rwx_is_copied_stripped_indexed_and_restored_without_execution() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let mut value = v2("code");
    value["entrypoints"]["backend"]["runtime"] = json!("executable");
    let source = package(&value);
    fs::set_permissions(
        source.path().join("backend.js"),
        fs::Permissions::from_mode(0o6755),
    )
    .unwrap();
    let pm = PackageManager::new(
        root.path().into(),
        vec![],
        "0.1.0".into(),
        "linux-x86_64".into(),
    );
    let old = pm.install(source.path(), true).unwrap();
    assert_eq!(
        fs::metadata(old.installed_path.join("backend.js"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    assert_eq!(
        old.files
            .iter()
            .find(|f| f.relative_path == "backend.js")
            .unwrap()
            .unix_mode,
        Some(0o755)
    );
    assert!(!old.installed_path.join("EXECUTED").exists());
    let expected = identity(&old.installed_path);
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Update,
        JournalPhase::OldMoved,
        &old.installed_path,
        Some((&old.installed_path, &old.digest)),
    );
    fs::rename(&old.installed_path, txn.join("backup")).unwrap();
    pm.list().unwrap();
    assert_eq!(identity(&old.installed_path), expected);
    fs::set_permissions(
        source.path().join("backend.js"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_ne!(pm.preview(source.path()).unwrap().digest, old.digest);
    fs::set_permissions(
        old.installed_path.join("backend.js"),
        fs::Permissions::from_mode(0o4755),
    )
    .unwrap();
    assert_eq!(
        pm.list()
            .unwrap()
            .iter()
            .find(|i| i.id == "code")
            .unwrap()
            .state,
        InventoryState::Invalid
    );
    pm.remove("code").unwrap();
}

#[test]
fn recovery_cannot_remove_builtin_id_through_a_forged_committed_journal() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let native = root.path().join("plugins/news");
    fs::create_dir_all(&native).unwrap();
    fs::write(native.join("manifest.json"), b"native sentinel").unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Remove,
        JournalPhase::RemoveCommitted,
        &native,
        None,
    );
    let path = txn.join("journal.json");
    let mut j: TransactionJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    j.plugin_id = "news".into();
    fs::write(&path, serde_json::to_vec(&j).unwrap()).unwrap();
    fs::rename(&native, txn.join("backup")).unwrap();
    assert_eq!(
        pm.list().unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
    assert_eq!(
        fs::read(txn.join("backup/manifest.json")).unwrap(),
        b"native sentinel"
    );
}

#[test]
fn committed_remove_recovery_finishes_only_recorded_backup() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Remove,
        JournalPhase::RemoveCommitted,
        &old.installed_path,
        None,
    );
    fs::rename(&old.installed_path, txn.join("backup")).unwrap();
    pm.list().unwrap();
    assert!(!old.installed_path.exists());
    assert!(!txn.exists());
}
#[test]
fn source_and_control_links_are_rejected_without_following_sentinel() {
    let root = tempfile::tempdir().unwrap();
    let source = package(&v2("code"));
    let sentinel = tempfile::tempdir().unwrap();
    fs::write(sentinel.path().join("secret"), b"untouched").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(sentinel.path().join("secret"), source.path().join("linked"))
            .unwrap();
    }
    #[cfg(windows)]
    {
        // Junctions require no symlink privilege. Never execute packaged code.
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(source.path().join("linked"))
            .arg(sentinel.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "junction fixture failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        manager(root.path())
            .install(source.path(), true)
            .unwrap_err()
            .code,
        PluginErrorCode::EntryInvalid
    );
    assert_eq!(
        fs::read(sentinel.path().join("secret")).unwrap(),
        b"untouched"
    );
    let control_root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(sentinel.path(), control_root.path().join("plugins")).unwrap();
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(control_root.path().join("plugins"))
            .arg(sentinel.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "junction fixture failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        manager(control_root.path())
            .remove("code")
            .unwrap_err()
            .code,
        PluginErrorCode::EntryInvalid
    );
    assert_eq!(
        fs::read(sentinel.path().join("secret")).unwrap(),
        b"untouched"
    );
}

#[test]
fn capturing_wal_recovers_intent_only_partial_and_complete_unindexed_files() {
    // Label-only cleanup would delete injected files; requiring a complete hash would strand every interrupted copy.
    for payload in [
        None,
        Some(b"part".as_slice()),
        Some(b"complete but not final-indexed".as_slice()),
    ] {
        let root = tempfile::tempdir().unwrap();
        let pm = manager(root.path());
        let source = package(&v2("code"));
        let old = pm.install(source.path(), true).unwrap();
        let expected = identity(&old.installed_path);
        let txn = journal_fixture(
            root.path(),
            JournalOperation::Update,
            JournalPhase::Capturing,
            &old.installed_path,
            None,
        );
        fs::create_dir(txn.join("stage")).unwrap();
        let path = txn.join("journal.json");
        let mut j: TransactionJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        j.capture_intents = vec![
            CaptureIntent {
                relative_path: "assets".into(),
                kind: CaptureKind::Directory,
                max_bytes: 0,
            },
            CaptureIntent {
                relative_path: "assets/copy".into(),
                kind: CaptureKind::File,
                max_bytes: 64 * 1024 * 1024,
            },
        ];
        fs::write(&path, serde_json::to_vec(&j).unwrap()).unwrap();
        if let Some(payload) = payload {
            fs::create_dir(txn.join("stage/assets")).unwrap();
            fs::write(txn.join("stage/assets/copy"), payload).unwrap();
        }
        pm.list().unwrap();
        assert_eq!(identity(&old.installed_path), expected);
        assert!(!txn.exists());
    }
}
#[test]
fn capturing_wal_preserves_unrecorded_sentinel_and_promotes_only_valid_pending_records() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Update,
        JournalPhase::Capturing,
        &old.installed_path,
        None,
    );
    fs::create_dir(txn.join("stage")).unwrap();
    fs::write(txn.join("stage/injected"), b"sentinel").unwrap();
    assert_eq!(
        pm.list().unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
    assert_eq!(fs::read(txn.join("stage/injected")).unwrap(), b"sentinel");
    // A valid write-ahead next record may be present before its rename. Its custody authorizes only this exact file.
    let mut j: TransactionJournal =
        serde_json::from_slice(&fs::read(txn.join("journal.json")).unwrap()).unwrap();
    j.capture_intents.push(CaptureIntent {
        relative_path: "injected".into(),
        kind: CaptureKind::File,
        max_bytes: 64 * 1024 * 1024,
    });
    fs::write(
        txn.join("journal.next.json"),
        serde_json::to_vec(&j).unwrap(),
    )
    .unwrap();
    pm.list().unwrap();
    assert!(!txn.exists());
}
#[test]
fn journal_candidate_must_bind_receipt_index_not_just_candidate_digest() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Update,
        JournalPhase::Published,
        &old.installed_path,
        Some((&old.installed_path, &old.digest)),
    );
    let path = txn.join("journal.json");
    let mut j: TransactionJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    j.candidate_files[0].sha256 = "0".repeat(64);
    fs::write(&path, serde_json::to_vec(&j).unwrap()).unwrap();
    assert_eq!(
        pm.list().unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
    assert!(old.installed_path.exists());
    assert!(txn.exists());
}
#[test]
fn actual_asset_tampering_invalidates_receipt_but_removal_can_repair() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let installed = pm.install(source.path(), true).unwrap();
    fs::write(
        installed.installed_path.join("ui/index.html"),
        b"<p>Tampered</p>",
    )
    .unwrap();
    assert_eq!(
        pm.list()
            .unwrap()
            .iter()
            .find(|i| i.id == "code")
            .unwrap()
            .state,
        InventoryState::Invalid
    );
    let incoming = package(&v1("other"));
    assert_eq!(
        pm.install(incoming.path(), true).unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
    pm.remove("code").unwrap();
}
#[test]
fn lexical_entry_attacks_and_non_html_assets_cannot_publish() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    for entry in [
        "missing.html",
        "/absolute.html",
        "../parent.html",
        "dir\\entry.html",
        "%2e%2e/entry.html",
        "//server/share.html",
        "entry.html:ads",
        "COM¹.html",
    ] {
        let mut value = v2("code");
        value["entrypoints"]["frontend"] = json!(entry);
        let source = package(&value);
        assert_eq!(
            pm.install(source.path(), true).unwrap_err().code,
            PluginErrorCode::EntryInvalid,
            "{entry}"
        );
        assert!(!root.path().join("plugins/code").exists());
    }
    let source = package(&v2("code"));
    fs::write(source.path().join("ui/index.html"), b"not an HTML asset").unwrap();
    assert_eq!(
        pm.install(source.path(), true).unwrap_err().code,
        PluginErrorCode::EntryInvalid
    );
}
#[test]
fn update_excludes_only_old_self_and_still_checks_other_reverse_declarations() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    pm.install(source.path(), true).unwrap();
    let mut other = v2("other");
    other["conflicts"] = json!([{"id":"code","reason":"other reverse"}]);
    // Existing legacy folders can predate conflict enforcement; all must participate in update.
    let source = package(&other);
    fs::create_dir(root.path().join("plugins/other")).unwrap();
    for entry in ["manifest.json", "backend.js"] {
        fs::copy(
            source.path().join(entry),
            root.path().join("plugins/other").join(entry),
        )
        .unwrap();
    }
    fs::create_dir(root.path().join("plugins/other/ui")).unwrap();
    fs::copy(
        source.path().join("ui/index.html"),
        root.path().join("plugins/other/ui/index.html"),
    )
    .unwrap();
    let before = identity(&root.path().join("plugins/code"));
    let source = package(&v2("code"));
    let error = pm.update(source.path(), true).unwrap_err();
    assert_eq!(error.code, PluginErrorCode::Conflict);
    assert_eq!(error.conflicts[0].declared_by, "other");
    assert_eq!(identity(&root.path().join("plugins/code")), before);
}
#[cfg(windows)]
#[test]
fn actual_directory_sharing_obstruction_fails_update_before_rename_with_old_tree_retained() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let installed = pm.install(source.path(), true).unwrap();
    let before = identity(&installed.installed_path);
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(0x02000000)
        .open(&installed.installed_path)
        .unwrap();
    let mut value = v2("code");
    value["version"] = json!("2.0.0");
    let update = package(&value);
    assert_eq!(
        pm.update(update.path(), true).unwrap_err().code,
        PluginErrorCode::Io
    );
    assert_eq!(identity(&installed.installed_path), before);
    drop(handle);
    pm.list().unwrap();
}

fn intents_for(files: &[PackageFile], dirs: &[String]) -> Vec<CaptureIntent> {
    let mut intents: Vec<_> = dirs
        .iter()
        .map(|p| CaptureIntent {
            relative_path: p.clone(),
            kind: CaptureKind::Directory,
            max_bytes: 0,
        })
        .chain(files.iter().map(|f| CaptureIntent {
            relative_path: f.relative_path.clone(),
            kind: CaptureKind::File,
            max_bytes: if f.relative_path == "manifest.json" {
                256 * 1024
            } else if f.relative_path == ".kaleid-receipt.json" {
                16 * 1024 * 1024
            } else {
                64 * 1024 * 1024
            },
        }))
        .collect();
    intents.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    intents
}
#[test]
fn pending_prepared_record_cannot_authorize_unrecorded_capture_files() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Update,
        JournalPhase::Capturing,
        &old.installed_path,
        None,
    );
    let stage = txn.join("stage");
    fs::create_dir(&stage).unwrap();
    let (files, dirs) = identity(&old.installed_path);
    for dir in &dirs {
        fs::create_dir_all(stage.join(dir)).unwrap();
    }
    for file in &files {
        fs::copy(
            old.installed_path.join(&file.relative_path),
            stage.join(&file.relative_path),
        )
        .unwrap();
    }
    let path = txn.join("journal.json");
    let mut j: TransactionJournal = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    j.capture_intents = intents_for(&files, &dirs);
    fs::write(&path, serde_json::to_vec(&j).unwrap()).unwrap();
    fs::write(stage.join("injected"), b"unrecorded sentinel").unwrap();
    let (candidate_files, candidate_directories) = identity(&stage);
    let mut next = j;
    next.phase = JournalPhase::Prepared;
    next.candidate_digest = Some(old.digest);
    next.candidate_files = candidate_files;
    next.candidate_directories = candidate_directories;
    fs::write(
        txn.join("journal.next.json"),
        serde_json::to_vec(&next).unwrap(),
    )
    .unwrap();
    assert_eq!(
        pm.list().unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
    assert_eq!(
        fs::read(stage.join("injected")).unwrap(),
        b"unrecorded sentinel"
    );
    assert!(old.installed_path.exists());
}

#[test]
fn pending_journal_cannot_rebind_already_prepared_candidate_identity() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Update,
        JournalPhase::OldMoved,
        &old.installed_path,
        Some((&old.installed_path, &old.digest)),
    );
    let backup_expected = identity(&old.installed_path);
    fs::rename(&old.installed_path, txn.join("backup")).unwrap();
    let other_root = tempfile::tempdir().unwrap();
    let mut value = v2("code");
    value["version"] = json!("2.0.0");
    let source = package(&value);
    let foreign = manager(other_root.path())
        .install(source.path(), true)
        .unwrap();
    let (files, dirs) = identity(&foreign.installed_path);
    fs::rename(&foreign.installed_path, &old.installed_path).unwrap();
    let mut next: TransactionJournal =
        serde_json::from_slice(&fs::read(txn.join("journal.json")).unwrap()).unwrap();
    next.candidate_digest = Some(foreign.digest);
    next.candidate_files = files;
    next.candidate_directories = dirs;
    fs::write(
        txn.join("journal.next.json"),
        serde_json::to_vec(&next).unwrap(),
    )
    .unwrap();
    assert_eq!(
        pm.list().unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
    assert_eq!(identity(&txn.join("backup")), backup_expected);
    assert_eq!(bytes(root.path(), "code"), value.to_string().as_bytes());
}

#[test]
fn journal_enums_require_string_wire_values_not_externally_tagged_objects() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Remove,
        JournalPhase::Prepared,
        &old.installed_path,
        None,
    );
    let path = txn.join("journal.json");
    let original = fs::read_to_string(&path).unwrap();
    for bad in [
        original.replace("\"phase\":\"prepared\"", "\"phase\":{\"prepared\":null}"),
        original.replace(
            "\"operation\":\"remove\"",
            "\"operation\":{\"remove\":null}",
        ),
    ] {
        fs::write(&path, bad).unwrap();
        assert_eq!(
            pm.list().unwrap_err().code,
            PluginErrorCode::InventoryInvalid
        );
        assert!(old.installed_path.exists());
    }
}

#[test]
fn oversized_control_record_fails_closed_before_parsing_or_removing_packages() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("code"));
    let old = pm.install(source.path(), true).unwrap();
    let txn = journal_fixture(
        root.path(),
        JournalOperation::Remove,
        JournalPhase::Prepared,
        &old.installed_path,
        None,
    );
    let path = txn.join("journal.json");
    let mut text = fs::read(&path).unwrap();
    text.resize(16 * 1024 * 1024 + 1, b' ');
    fs::write(&path, text).unwrap();
    assert_eq!(
        pm.list().unwrap_err().code,
        PluginErrorCode::InventoryInvalid
    );
    assert_eq!(
        bytes(root.path(), "code"),
        v2("code").to_string().as_bytes()
    );
    assert!(txn.exists());
}

#[test]
fn corrupt_oversize_manifest_remains_listed_and_can_be_removed() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("plugins/broken")).unwrap();
    fs::write(
        root.path().join("plugins/broken/manifest.json"),
        vec![b'{'; 262145],
    )
    .unwrap();
    let pm = manager(root.path());
    assert_eq!(
        pm.list()
            .unwrap()
            .iter()
            .find(|i| i.id == "broken")
            .unwrap()
            .state,
        InventoryState::Invalid
    );
    pm.remove("broken").unwrap();
    assert!(!root.path().join("plugins/broken").exists());
}
#[test]
fn exact_payload_total_limit_excludes_installer_receipt_overhead() {
    let root = tempfile::tempdir().unwrap();
    let source = package(&v2("code"));
    let small_bytes = fs::metadata(source.path().join("manifest.json"))
        .unwrap()
        .len()
        + fs::metadata(source.path().join("ui/index.html"))
            .unwrap()
            .len()
        + fs::metadata(source.path().join("backend.js"))
            .unwrap()
            .len();
    for (name, size) in [
        ("a", 64 * 1024 * 1024),
        ("b", 64 * 1024 * 1024),
        ("c", 64 * 1024 * 1024),
        ("d", 64 * 1024 * 1024 - small_bytes),
    ] {
        fs::File::create(source.path().join(name))
            .unwrap()
            .set_len(size)
            .unwrap();
    }
    let pm = manager(root.path());
    let installed = pm.install(source.path(), true).unwrap();
    assert_eq!(
        installed.files.iter().map(|f| f.bytes).sum::<u64>(),
        256 * 1024 * 1024
    );
    assert_eq!(
        pm.list()
            .unwrap()
            .iter()
            .find(|i| i.id == "code")
            .unwrap()
            .state,
        InventoryState::Ready
    );
    pm.remove("code").unwrap();
}

#[test]
fn actual_install_lock_obstruction_leaves_existing_update_bytes_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let pm = manager(root.path());
    let source = package(&v2("old"));
    pm.install(source.path(), true).unwrap();
    let before = bytes(root.path(), "old");
    fs::remove_file(root.path().join(".plugin-install.lock")).unwrap();
    fs::create_dir(root.path().join(".plugin-install.lock")).unwrap();
    assert!(pm.update(source.path(), true).is_err());
    assert_eq!(bytes(root.path(), "old"), before);
}

#[test]
fn simultaneous_conflicting_installs_cannot_both_publish() {
    let root = tempfile::tempdir().unwrap();
    let a = package(&v2("a"));
    let mut value = v2("b");
    value["conflicts"] = json!([{"id":"a","reason":"exclusive"}]);
    let b = package(&value);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [a.path(), b.path()]
        .iter()
        .map(|path| {
            let pm = manager(root.path());
            let path = path.to_path_buf();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                pm.install(&path, true)
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results.iter().find_map(|r| r.as_ref().err()).unwrap().code,
        PluginErrorCode::Conflict
    );
    assert_eq!(
        manager(root.path())
            .list()
            .unwrap()
            .iter()
            .filter(|i| i.state == InventoryState::Ready)
            .count(),
        1
    );
}
