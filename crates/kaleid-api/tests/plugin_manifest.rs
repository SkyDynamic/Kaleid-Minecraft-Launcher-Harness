//! Public contract tests: each table names inputs a missing validation branch would accept.
use kaleid_api::plugins::{BackendRuntime, ManifestStep, ManifestV1, PluginPlatform, UiMode};
use kaleid_api::{
    decode_manifest, parse_manifest, validate_manifest, validate_manifest_structure,
    validate_relative_entry, ApiError, PluginError, PluginErrorCode, PluginManifest, APP_VERSION,
};
use serde_json::{json, Value};

fn frontend() -> Value {
    json!({"id":"sample", "name":"Sample", "version":"1.0.0", "api":"2",
        "appVersion":">=0.1.0, <0.2.0", "conflicts":[],
        "entrypoints":{"frontend":"ui/index.html"}})
}

fn backend() -> Value {
    json!({"id":"sample", "name":"Sample", "version":"1.0.0", "api":"2",
        "appVersion":">=0.1.0, <0.2.0", "conflicts":[],
        "entrypoints":{"backend":{"runtime":"executable", "entry":"bin/backend.exe"}}})
}

fn command() -> Value {
    json!({"name":"hello_1-X", "inputSchema":{"type":"object"}, "outputSchema":true})
}

fn parsed(value: &Value) -> Result<PluginManifest, PluginError> {
    parse_manifest(&value.to_string(), "0.1.0")
}

fn rejects(value: &Value, code: PluginErrorCode) -> PluginError {
    let error = parsed(value).expect_err("invalid declaration was accepted");
    assert_eq!(error.code, code, "{value}: {error:?}");
    error
}

#[test]
fn existing_v1_examples_keep_their_exact_declarative_shape() {
    let fixtures = [
        (
            include_str!("../../../plugins/example-panel/manifest.json"),
            "example-panel",
            1,
        ),
        (
            include_str!("../../../plugins/example-settings/manifest.json"),
            "example-settings",
            2,
        ),
        (
            include_str!("../../../plugins/example-mirror/manifest.json"),
            "example-mirror",
            2,
        ),
    ];
    for (text, id, steps) in fixtures {
        let manifest = parse_manifest(text, APP_VERSION).unwrap();
        assert!(matches!(manifest, PluginManifest::V1(_)));
        assert_eq!(manifest.id(), id);
        assert_eq!(
            manifest.name(),
            serde_json::from_str::<Value>(text).unwrap()["name"]
        );
        assert_eq!(manifest.version(), "1.0.0");
        assert_eq!(manifest.api(), "1");
        assert_eq!(manifest.steps().len(), steps);
        let encoded = serde_json::to_value(&manifest).unwrap();
        assert_eq!(encoded["id"], id);
        assert_eq!(
            encoded["steps"],
            serde_json::from_str::<Value>(text).unwrap()["steps"]
        );
        assert!(encoded.get("V1").is_none());
        assert!(encoded.get("appVersion").is_none());
    }
}

#[test]
fn legacy_constructor_and_non_semver_versions_remain_valid() {
    let legacy = PluginManifest::V1(ManifestV1 {
        id: "old-plugin".into(),
        name: "Old".into(),
        version: "release seven".into(),
        api: "1".into(),
        author: None,
        signature: Some("opaque signature".into()),
        steps: vec![ManifestStep::DisablePlugin {
            target: "unknown-business-target".into(),
        }],
    });
    validate_manifest(&legacy, "not-a-semver").unwrap();
    let encoded = serde_json::to_string(&legacy).unwrap();
    assert_eq!(
        parse_manifest(&encoded, APP_VERSION).unwrap().version(),
        "release seven"
    );
}

#[test]
fn v1_requires_steps_and_refuses_v2_fields_and_unknown_step_parameters() {
    let base = json!({"id":"old", "name":"Old", "version":"release seven", "api":"1",
        "steps":[{"op":"enable-plugin", "target":"arbitrary-business-target"}]});
    for extra in ["appVersion", "entrypoints", "surprise"] {
        let mut value = base.clone();
        value[extra] = json!({});
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("steps");
    rejects(&missing, PluginErrorCode::InvalidManifest);
    for step in [
        json!({"op":"exec","cmd":"calc"}),
        json!({"op":"eval","text":"code"}),
        json!({"op":"enable-plugin","target":"x","params":{}}),
        json!({"op":"enable-plugin","target":"x","loop":1}),
    ] {
        let mut value = base.clone();
        value["steps"] = json!([step]);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
}

#[test]
fn v2_frontend_defaults_and_serialization_are_public_unwrapped_camel_case() {
    let manifest = parsed(&frontend()).unwrap();
    let PluginManifest::V2(v2) = &manifest else {
        panic!("wrong API branch")
    };
    assert_eq!(v2.entrypoints.frontend.as_deref(), Some("ui/index.html"));
    assert!(v2.platforms.is_empty());
    assert!(v2.steps.is_empty());
    assert!(v2.replaces.is_empty());
    assert!(v2.permissions.host_commands.is_empty());
    assert!(v2.contributes.ui.is_empty());
    let encoded = serde_json::to_value(&manifest).unwrap();
    assert_eq!(encoded["api"], "2");
    assert_eq!(encoded["appVersion"], ">=0.1.0, <0.2.0");
    assert_eq!(encoded["conflicts"], json!([]));
    assert_eq!(encoded["permissions"]["hostCommands"], json!([]));
    assert!(encoded.get("V2").is_none());
    assert!(encoded.get("app_version").is_none());
    parsed(&encoded).unwrap();
}

#[test]
fn backend_only_accepts_each_explicit_runtime_and_default_args() {
    for (name, runtime) in [
        ("executable", BackendRuntime::Executable),
        ("node", BackendRuntime::Node),
        ("python", BackendRuntime::Python),
        ("java", BackendRuntime::Java),
    ] {
        let mut value = backend();
        value["entrypoints"]["backend"]["runtime"] = json!(name);
        let PluginManifest::V2(v2) = parsed(&value).unwrap() else {
            panic!("wrong API branch")
        };
        let backend = v2.entrypoints.backend.unwrap();
        assert_eq!(backend.runtime, runtime);
        assert!(backend.args.is_empty());
    }
}

#[test]
fn declaration_only_v2_accepts_steps_and_optional_null_entrypoints() {
    let mut value = frontend();
    value["entrypoints"] = json!({"frontend":null,"backend":null});
    value["steps"] = json!([{"op":"set-default-setting","key":"futureSetting","value":null}]);
    let manifest = parsed(&value).unwrap();
    assert_eq!(manifest.steps().len(), 1);
    value["steps"] = json!([]);
    rejects(&value, PluginErrorCode::InvalidManifest);
}

#[test]
fn v2_requires_api_app_version_and_explicit_conflicts() {
    for field in ["api", "appVersion", "conflicts"] {
        let mut value = frontend();
        value.as_object_mut().unwrap().remove(field);
        let error = rejects(&value, PluginErrorCode::InvalidManifest);
        if field != "api" {
            assert_eq!(error.plugin_id.as_deref(), Some("sample"));
        }
    }
    let mut value = frontend();
    value["api"] = json!("3");
    let error = rejects(&value, PluginErrorCode::UnsupportedApi);
    assert_eq!(error.plugin_id.as_deref(), Some("sample"));
    value["api"] = json!(2);
    rejects(&value, PluginErrorCode::InvalidManifest);
}

#[test]
fn malformed_json_and_non_object_roots_fail_without_panics() {
    for text in [
        "{",
        "",
        "null",
        "[]",
        "true",
        "{} trailing",
        "{\"api\":null}",
    ] {
        assert_eq!(
            parse_manifest(text, "0.1.0").unwrap_err().code,
            PluginErrorCode::InvalidManifest
        );
    }
}

#[test]
fn deny_unknown_fields_at_every_known_structural_level() {
    let mut all = backend();
    all["entrypoints"]["frontend"] = json!("index.html");
    all["conflicts"] = json!([{"id":"other","reason":"different provider"}]);
    all["contributes"] = json!({"ui":[{"target":"future.panel","mode":"append"}],
        "commands":[command()], "services":[{"name":"data","contractVersion":1,"methods":[command()]}]});
    all["permissions"] = json!({"hostCommands":["host.future"],"events":["future.event"]});
    all["steps"] = json!([{"op":"update-config","target":"future","patch":{}}]);
    parsed(&all).unwrap();
    for pointer in [
        "",
        "/entrypoints",
        "/entrypoints/backend",
        "/conflicts/0",
        "/contributes",
        "/contributes/ui/0",
        "/contributes/commands/0",
        "/contributes/services/0",
        "/contributes/services/0/methods/0",
        "/permissions",
        "/steps/0",
    ] {
        let mut value = all.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), json!(true));
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    for (pointer, bad) in [
        ("/entrypoints/backend/runtime", "shell"),
        ("/contributes/ui/0/mode", "inject"),
    ] {
        let mut value = all.clone();
        *value.pointer_mut(pointer).unwrap() = json!(bad);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
}

#[test]
fn positional_arrays_cannot_replace_known_json_objects() {
    for (pointer, array) in [
        ("/entrypoints", json!(["index.html", null])),
        (
            "/entrypoints/backend",
            json!(["executable", "bin/backend.exe", []]),
        ),
        ("/conflicts/0", json!(["other", "different provider"])),
        ("/contributes", json!([[], [], []])),
        (
            "/contributes/ui/0",
            json!(["future.panel", "append", null, {}]),
        ),
        ("/contributes/commands/0", json!(["call", {}, true])),
        ("/contributes/services/0", json!(["data", 1, [command()]])),
        (
            "/contributes/services/0/methods/0",
            json!(["call", {}, true]),
        ),
        ("/permissions", json!([[], []])),
    ] {
        let mut value = backend();
        value["entrypoints"]["frontend"] = json!("index.html");
        value["conflicts"] = json!([{"id":"other","reason":"different provider"}]);
        value["contributes"] = json!({"ui":[{"target":"future.panel","mode":"append"}],
            "commands":[command()], "services":[{"name":"data","contractVersion":1,"methods":[command()]}]});
        value["permissions"] = json!({});
        *value.pointer_mut(pointer).unwrap() = array;
        let error = decode_manifest(&value.to_string()).expect_err(pointer);
        assert_eq!(error.code, PluginErrorCode::InvalidManifest, "{pointer}");
    }
}

#[test]
fn object_shape_checks_leave_arbitrary_json_payloads_unchanged() {
    let mut value = backend();
    value["entrypoints"]["frontend"] = json!("index.html");
    value["contributes"] = json!({"ui":[{"target":"future.panel","mode":"append","config":["arbitrary",{}]}],
        "commands":[{"name":"call","inputSchema":{"futureKeyword":[1,2]},"outputSchema":true}]});
    value["steps"] = json!([
        {"op":"update-config","target":"future","patch":{"list":[1,2]}},
        {"op":"set-default-setting","key":"futureSetting","value":[true,{}]}
    ]);
    let encoded = serde_json::to_value(parsed(&value).unwrap()).unwrap();
    assert_eq!(
        encoded["contributes"]["ui"][0]["config"],
        json!(["arbitrary", {}])
    );
    assert_eq!(
        encoded["contributes"]["commands"][0]["inputSchema"],
        json!({"futureKeyword":[1,2]})
    );
    assert_eq!(encoded["steps"][0]["patch"], json!({"list":[1,2]}));
    assert_eq!(encoded["steps"][1]["value"], json!([true, {}]));
}

#[test]
fn duplicate_known_json_fields_are_not_silently_overwritten() {
    for text in [
        r#"{"api":"2","api":"1"}"#,
        r#"{"id":"x","name":"X","name":"Y","version":"1.0.0","api":"2","appVersion":"*","conflicts":[],"entrypoints":{"frontend":"index.html"}}"#,
        r#"{"id":"x","name":"X","version":"1.0.0","api":"2","appVersion":"*","conflicts":[],"entrypoints":{"frontend":"index.html","frontend":"evil.html"}}"#,
        r#"{"id":"x","name":"X","version":"1.0.0","api":"1","steps":[{"op":"enable-plugin","target":"x","target":"y"}]}"#,
    ] {
        assert_eq!(
            decode_manifest(text).unwrap_err().code,
            PluginErrorCode::InvalidManifest
        );
    }
}

#[test]
fn manifest_limit_counts_utf8_bytes_before_json_decoding() {
    let value = frontend();
    let text = value.to_string();
    let exact = format!("{}{}", text, " ".repeat(262144 - text.len()));
    parse_manifest(&exact, "0.1.0").unwrap();
    let over = format!("{exact} ");
    assert_eq!(
        decode_manifest(&over).unwrap_err().code,
        PluginErrorCode::PackageTooLarge
    );
    let unicode = format!("{{{}", "界".repeat(90000)); // <256Ki characters, >256Ki bytes; invalid JSON too.
    assert_eq!(
        decode_manifest(&unicode).unwrap_err().code,
        PluginErrorCode::PackageTooLarge
    );
}

#[test]
fn compatibility_uses_semver_and_identifies_candidate_current_and_range() {
    for version in ["0.2.0", "0.0.9"] {
        let error = parse_manifest(&frontend().to_string(), version).unwrap_err();
        assert_eq!(error.code, PluginErrorCode::AppVersionMismatch);
        assert_eq!(error.plugin_id.as_deref(), Some("sample"));
        assert_eq!(error.app_version.as_deref(), Some(version));
        assert_eq!(error.required_range.as_deref(), Some(">=0.1.0, <0.2.0"));
        assert!(error.outcome_known);
    }
    parse_manifest(&frontend().to_string(), "0.1.0").unwrap();
}

#[test]
fn invalid_ranges_host_versions_and_v2_versions_have_typed_errors() {
    for range in ["", " ", "not semver", ">=0.1.0 || <0.2.0"] {
        let mut value = frontend();
        value["appVersion"] = json!(range);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    let error = parse_manifest(&frontend().to_string(), "not-version").unwrap_err();
    assert_eq!(error.code, PluginErrorCode::InvalidManifest);
    assert_eq!(error.plugin_id.as_deref(), Some("sample"));
    assert_eq!(error.app_version.as_deref(), Some("not-version"));
    for version in ["", "1", "v1.0.0", "01.0.0", "release seven", " 1.0.0 "] {
        let mut value = frontend();
        value["version"] = json!(version);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
}

#[test]
fn prerelease_hosts_match_only_explicit_prerelease_requirements() {
    let mut value = frontend();
    value["appVersion"] = json!(">=0.1.0, <0.2.0");
    assert_eq!(
        parse_manifest(&value.to_string(), "0.1.1-beta.1")
            .unwrap_err()
            .code,
        PluginErrorCode::AppVersionMismatch
    );
    value["appVersion"] = json!(">=0.1.1-beta.1, <0.2.0");
    parse_manifest(&value.to_string(), "0.1.1-beta.2").unwrap();
}

#[test]
fn inventory_can_decode_and_validate_intrinsic_rules_despite_incompatible_app() {
    let mut value = backend();
    value["appVersion"] = json!(">=0.2.0");
    value["conflicts"] = json!([{"id":"competing","reason":"owns same source"}]);
    let manifest = decode_manifest(&value.to_string()).unwrap();
    assert_eq!(manifest.id(), "sample");
    assert_eq!(manifest.conflicts()[0].id, "competing");
    validate_manifest_structure(&manifest).unwrap();
    assert_eq!(
        validate_manifest(&manifest, APP_VERSION).unwrap_err().code,
        PluginErrorCode::AppVersionMismatch
    );
    value["conflicts"] = json!([{"id":"sample","reason":"self"}]);
    let malformed = decode_manifest(&value.to_string()).unwrap();
    assert_eq!(
        validate_manifest_structure(&malformed).unwrap_err().code,
        PluginErrorCode::InvalidManifest
    );
    assert_eq!(
        validate_manifest(&malformed, APP_VERSION).unwrap_err().code,
        PluginErrorCode::InvalidManifest
    );
}

#[test]
fn ids_use_ascii_and_exact_48_byte_boundary_and_names_are_nonempty() {
    let mut value = frontend();
    value["id"] = json!("a".repeat(48));
    parsed(&value).unwrap();
    for id in [
        "a".repeat(49),
        "".into(),
        "Bad".into(),
        "bad_id".into(),
        "中".into(),
        "with space".into(),
    ] {
        value["id"] = json!(id);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    value["id"] = json!("sample");
    for name in ["", " \t\n"] {
        value["name"] = json!(name);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
}

#[test]
fn conflicts_require_valid_unique_other_ids_and_bounded_unicode_reasons() {
    let mut value = backend();
    value["conflicts"] = json!([{"id":"other","reason":"界".repeat(512)}]);
    parsed(&value).unwrap();
    for conflicts in [
        json!([{"id":"other","reason":"界".repeat(513)}]),
        json!([{"id":"other","reason":" \n"}]),
        json!([{"id":"Bad","reason":"x"}]),
        json!([{"id":"sample","reason":"x"}]),
        json!([{"id":"other","reason":"a"},{"id":"other","reason":"b"}]),
    ] {
        value["conflicts"] = conflicts;
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
}

#[test]
fn replacements_reject_self_duplicates_overlap_and_protected_host_targets_only() {
    let mut value = backend();
    value["replaces"] = json!(["future-business-plugin"]);
    parsed(&value).unwrap();
    for ids in [json!(["sample"]), json!(["Bad"]), json!(["other", "other"])] {
        value["replaces"] = ids;
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    for id in [
        "kernel",
        "plugin-manager",
        "launcher-logger",
        "process-broker",
        "app-identity",
        "native-permissions",
        "recovery",
    ] {
        value["replaces"] = json!([id]);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    value["replaces"] = json!(["other"]);
    value["conflicts"] = json!([{"id":"other","reason":"exclusive"}]);
    rejects(&value, PluginErrorCode::InvalidManifest);
}

#[test]
fn command_and_method_names_are_unique_segments_and_schemas_object_or_boolean() {
    let mut value = backend();
    value["contributes"] = json!({"commands":[command()]});
    parsed(&value).unwrap();
    value["contributes"]["commands"][0]["inputSchema"] = json!(false);
    value["contributes"]["commands"][0]["outputSchema"] = json!({});
    parsed(&value).unwrap();
    for name in [
        "",
        " ",
        "plugin.sample.call",
        "host.call",
        "bad/name",
        "中文",
        "call*",
    ] {
        let mut invalid = backend();
        let mut cmd = command();
        cmd["name"] = json!(name);
        invalid["contributes"] = json!({"commands":[cmd]});
        rejects(&invalid, PluginErrorCode::InvalidManifest);
    }
    for schema in [json!(null), json!([]), json!("object"), json!(3)] {
        for field in ["inputSchema", "outputSchema"] {
            let mut invalid = value.clone();
            invalid["contributes"]["commands"][0][field] = schema.clone();
            rejects(&invalid, PluginErrorCode::InvalidManifest);
        }
    }
    value["contributes"]["commands"] = json!([command(), command()]);
    rejects(&value, PluginErrorCode::InvalidManifest);
}

#[test]
fn service_contracts_require_positive_versions_nonempty_unique_valid_methods_and_names() {
    let service = json!({"name":"data","contractVersion":1,"methods":[command()]});
    let mut value = backend();
    value["contributes"] = json!({"services":[service.clone()]});
    parsed(&value).unwrap();
    for (field, invalid) in [
        ("name", json!("host.data")),
        ("name", json!("")),
        ("contractVersion", json!(0)),
        ("contractVersion", json!(-1)),
        ("contractVersion", json!(4294967296u64)),
        ("methods", json!([])),
        ("methods", json!([command(), command()])),
        (
            "methods",
            json!([{"name":"bad.method","inputSchema":true,"outputSchema":true}]),
        ),
    ] {
        let mut malformed = value.clone();
        malformed["contributes"]["services"][0][field] = invalid;
        rejects(&malformed, PluginErrorCode::InvalidManifest);
    }
    value["contributes"]["services"] = json!([service.clone(), service]);
    rejects(&value, PluginErrorCode::InvalidManifest);
}

#[test]
fn permissions_require_exact_trimmed_nonempty_names_without_duplicates_or_wildcards() {
    let mut value = frontend();
    value["permissions"] = json!({"hostCommands":["host.future"],"events":["future.changed"]});
    parsed(&value).unwrap();
    for field in ["hostCommands", "events"] {
        for names in [
            json!([""]),
            json!([" "]),
            json!([" host.name"]),
            json!(["host.name "]),
            json!(["*"]),
            json!(["host.*"]),
            json!(["host.name", "host.name"]),
        ] {
            let mut invalid = value.clone();
            invalid["permissions"][field] = names;
            rejects(&invalid, PluginErrorCode::InvalidManifest);
        }
    }
}

#[test]
fn contributions_require_their_frontend_or_backend_and_root_is_replace_only() {
    let mut ui = backend();
    ui["contributes"] = json!({"ui":[{"target":"future.panel","mode":"append"}]});
    rejects(&ui, PluginErrorCode::InvalidManifest);
    ui["entrypoints"]["frontend"] = json!("index.html");
    let PluginManifest::V2(v2) = parsed(&ui).unwrap() else {
        panic!("wrong API branch")
    };
    assert_eq!(v2.contributes.ui[0].mode, UiMode::Append);
    assert_eq!(v2.contributes.ui[0].config, json!({}));
    assert!(v2.contributes.ui[0].title.is_none());
    ui["contributes"]["ui"][0]["target"] = json!("app.root");
    rejects(&ui, PluginErrorCode::InvalidManifest);
    ui["contributes"]["ui"][0]["mode"] = json!("replace");
    parsed(&ui).unwrap();
    ui["contributes"]["ui"][0]["target"] = json!(" \t");
    rejects(&ui, PluginErrorCode::InvalidManifest);
    for contributions in [
        json!({"commands":[command()]}),
        json!({"services":[{"name":"data","contractVersion":1,"methods":[command()]}]}),
    ] {
        let mut value = frontend();
        value["contributes"] = contributions;
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    let mut value = frontend();
    value["replaces"] = json!(["future-business-plugin"]);
    rejects(&value, PluginErrorCode::InvalidManifest);
}

#[test]
fn platform_declarations_are_standard_unique_and_not_matched_to_current_os() {
    let mut value = frontend();
    value["platforms"] = json!([
        "windows-x86_64",
        "windows-x86",
        "windows-aarch64",
        "linux-x86_64",
        "linux-aarch64",
        "macos-x86_64",
        "macos-aarch64"
    ]);
    let PluginManifest::V2(v2) = parsed(&value).unwrap() else {
        panic!("wrong API branch")
    };
    assert_eq!(v2.platforms[0], PluginPlatform::WindowsX86_64);
    for platforms in [
        json!(["freebsd-x86_64"]),
        json!(["windows-x86_64", "windows-x86_64"]),
        json!(["Linux-x86_64"]),
    ] {
        value["platforms"] = platforms;
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
    value["platforms"] = json!(["macos-aarch64"]);
    parsed(&value).unwrap();
}

#[test]
fn every_closed_step_checks_its_declared_semantics_without_catalog_guesses() {
    let mut value = frontend();
    value["steps"] = json!([
        {"op":"update-config","target":"unknown-target","patch":{"futureField":1}},
        {"op":"disable-plugin","target":"kernel"}, {"op":"enable-plugin","target":"unknown-target"},
        {"op":"provide-source","id":"source-1","label":"Source","manifestUrl":"https://example/m.json","baseUrl":"https://example"},
        {"op":"bind-slot","slot":"unknown.slot","title":"Notice","view":"readonly-text:Notice"},
        {"op":"set-default-setting","key":"unknownSetting","value":false}
    ]);
    let manifest = parsed(&value).unwrap();
    assert_eq!(manifest.steps().len(), 6);
    let ManifestStep::ProvideSource { priority, .. } = manifest.steps()[3] else {
        panic!("wrong step")
    };
    assert_eq!(priority, 0);
    for step in [
        json!({"op":"update-config","target":"","patch":{}}),
        json!({"op":"update-config","target":"x","patch":[]}),
        json!({"op":"disable-plugin","target":" \t"}),
        json!({"op":"enable-plugin","target":""}),
        json!({"op":"provide-source","id":"Bad","label":"x","manifestUrl":"https://x","baseUrl":"https://x"}),
        json!({"op":"provide-source","id":"ok","label":"","manifestUrl":"https://x","baseUrl":"https://x"}),
        json!({"op":"provide-source","id":"ok","label":"x","manifestUrl":"http://x","baseUrl":"https://x"}),
        json!({"op":"provide-source","id":"ok","label":"x","manifestUrl":"https://x","baseUrl":"http://x"}),
        json!({"op":"bind-slot","slot":"","title":"x","view":"readonly-text:x"}),
        json!({"op":"bind-slot","slot":"x","title":"","view":"readonly-text:x"}),
        json!({"op":"bind-slot","slot":"x","title":"x","view":"js:alert(1)"}),
        json!({"op":"set-default-setting","key":" ","value":null}),
    ] {
        value["steps"] = json!([step]);
        rejects(&value, PluginErrorCode::InvalidManifest);
    }
}

#[test]
fn relative_entry_attack_table_rejects_noncanonical_and_windows_unsafe_paths() {
    for entry in [
        "",
        "/index.html",
        "\\index.html",
        "C:/index.html",
        "C:index.html",
        "//server/share/index.html",
        "\\\\server\\share\\index.html",
        "dir\\index.html",
        "dir//index.html",
        "dir/",
        "./index.html",
        "dir/./index.html",
        "../index.html",
        "dir/../index.html",
        "index.html:stream",
        "dir/%2e%2e/index.html",
        "%252e%252e/index.html",
        "100%.html",
        "dir/nu\0l.html",
        "dir/line\n.html",
        "dir/\u{0085}.html",
        "dir./index.html",
        "dir /index.html",
        "CON",
        "con.html",
        "PrN.txt",
        "AUX",
        "nul.exe",
        "COM1.html",
        "com9.exe",
        "LPT1",
        "lpt9.txt",
    ] {
        assert_eq!(
            validate_relative_entry(entry).unwrap_err().code,
            PluginErrorCode::EntryInvalid,
            "{entry:?}"
        );
    }
}

#[test]
fn strict_manifest_ingress_requires_string_only_unit_enum_wire_fields() {
    // Serde unit enums otherwise also accept externally tagged object encodings.
    let mut base = backend();
    base["entrypoints"]["frontend"] = json!("index.html");
    base["platforms"] = json!(["windows-x86_64"]);
    base["contributes"] =
        json!({"ui":[{"target":"app.root","mode":"replace","config":{"node":null}}]});
    decode_manifest(&base.to_string()).unwrap();
    let mut accepted = Vec::new();
    for (pointer, value, label) in [
        (
            "/entrypoints/backend/runtime",
            json!({"node":null}),
            "runtime object",
        ),
        (
            "/platforms/0",
            json!({"windows-x86_64":null}),
            "platform object",
        ),
        (
            "/contributes/ui/0/mode",
            json!({"replace":null}),
            "UI mode object",
        ),
    ] {
        let mut invalid = base.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        match decode_manifest(&invalid.to_string()) {
            Ok(_) => accepted.push(label),
            Err(error) => assert_eq!(error.code, PluginErrorCode::InvalidManifest),
        }
    }
    assert!(
        accepted.is_empty(),
        "noncanonical known enum shapes accepted: {accepted:?}"
    );
    let encoded = serde_json::to_value(parsed(&base).unwrap()).unwrap();
    assert_eq!(
        encoded["contributes"]["ui"][0]["config"],
        json!({"node":null})
    );
}

#[test]
fn portable_paths_reject_superscript_devices_and_windows_illegal_characters() {
    // Removing portable device/character checks would admit paths aliased by Windows.
    for entry in [
        "COM¹",
        "com².html",
        "CoM³.exe",
        "LPT¹",
        "lpt².txt",
        "LpT³.html",
        "ui/COM¹/index.html",
        "assets/lpt³.data",
        "bad<.html",
        "bad>.html",
        "bad\".html",
        "bad|.html",
        "bad?.html",
        "bad*.html",
    ] {
        assert_eq!(
            validate_relative_entry(entry).unwrap_err().code,
            PluginErrorCode::EntryInvalid,
            "{entry:?}"
        );
    }
    for entry in [
        "COM10.html",
        "lpt10.txt",
        "页面/主页.html",
        "assets/指数¹.txt",
    ] {
        validate_relative_entry(entry).unwrap();
    }
}

#[test]
fn relative_entry_positive_paths_do_not_require_filesystem_existence() {
    // These deliberately nonexistent names demonstrate lexical-only validation, not a filesystem probe.
    for entry in [
        "index.html",
        "ui/hello world.html",
        "页面/主页.html",
        "bin/backend.exe",
        "assets/.hidden",
        "com10.exe",
        "lpt0.txt",
        "connection.html",
        "dir/not-installed-674eb0c288cc.html",
    ] {
        validate_relative_entry(entry).unwrap();
    }
}

#[test]
fn frontend_requires_html_and_both_entrypoints_use_lexical_validation_with_candidate_id() {
    for entry in ["index.js", "ui/../index.html", "C:/index.html"] {
        let mut value = frontend();
        value["entrypoints"]["frontend"] = json!(entry);
        let error = rejects(&value, PluginErrorCode::EntryInvalid);
        assert_eq!(error.plugin_id.as_deref(), Some("sample"));
    }
    let mut value = backend();
    value["entrypoints"]["backend"]["entry"] = json!("../backend.exe");
    rejects(&value, PluginErrorCode::EntryInvalid);
}

#[test]
fn plugin_error_camel_case_roundtrip_and_default_known_outcome_are_stable() {
    let encoded = json!({"code":"conflict", "message":"Cannot install competing source", "pluginId":"sample",
        "conflicts":[{"id":"other","reason":"same provider","declaredBy":"sample"}],
        "appVersion":"0.1.0", "requiredRange":">=0.2.0", "outcomeKnown":false});
    let error: PluginError = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(error.code, PluginErrorCode::Conflict);
    assert!(!error.outcome_known);
    assert_eq!(error.conflicts[0].declared_by, "sample");
    assert_eq!(serde_json::to_value(&error).unwrap(), encoded);
    let minimal: PluginError =
        serde_json::from_value(json!({"code":"io","message":"Cannot read package"})).unwrap();
    assert!(minimal.outcome_known);
    assert!(minimal.conflicts.is_empty());
    assert_eq!(
        serde_json::to_value(&minimal).unwrap(),
        json!({"code":"io","message":"Cannot read package","conflicts":[],"outcomeKnown":true})
    );
    fn std_error_message(error: &dyn std::error::Error) -> String {
        error.to_string()
    }
    assert!(std_error_message(&minimal).contains("Cannot read package"));
    assert_eq!(
        serde_json::to_value(ApiError::Invalid("legacy".into())).unwrap(),
        json!("非法输入：legacy")
    );
}

#[test]
fn all_published_error_codes_are_snake_case_and_roundtrip() {
    for (code, wire) in [
        (PluginErrorCode::InvalidManifest, "invalid_manifest"),
        (PluginErrorCode::UnsupportedApi, "unsupported_api"),
        (PluginErrorCode::AppVersionMismatch, "app_version_mismatch"),
        (PluginErrorCode::Conflict, "conflict"),
        (PluginErrorCode::InventoryInvalid, "inventory_invalid"),
        (PluginErrorCode::EntryInvalid, "entry_invalid"),
        (PluginErrorCode::UnsupportedPlatform, "unsupported_platform"),
        (PluginErrorCode::PackageTooLarge, "package_too_large"),
        (
            PluginErrorCode::SignatureUnsupported,
            "signature_unsupported",
        ),
        (PluginErrorCode::UnsignedNotAllowed, "unsigned_not_allowed"),
        (PluginErrorCode::AlreadyInstalled, "already_installed"),
        (PluginErrorCode::Busy, "busy"),
        (PluginErrorCode::Unauthorized, "unauthorized"),
        (PluginErrorCode::RuntimeUnavailable, "runtime_unavailable"),
        (PluginErrorCode::Protocol, "protocol"),
        (PluginErrorCode::Timeout, "timeout"),
        (PluginErrorCode::BackendExited, "backend_exited"),
        (PluginErrorCode::Io, "io"),
    ] {
        let error = PluginError::new(code, "message").with_plugin_id("sample");
        let encoded = serde_json::to_value(&error).unwrap();
        assert_eq!(encoded["code"], wire);
        assert_eq!(
            serde_json::from_value::<PluginError>(encoded).unwrap(),
            error
        );
    }
}

#[test]
fn validating_manual_models_rechecks_api_variant_and_intrinsic_fields() {
    let mut manifest = parsed(&frontend()).unwrap();
    let PluginManifest::V2(v2) = &mut manifest else {
        panic!("wrong API branch")
    };
    v2.api = "1".into();
    assert_eq!(
        validate_manifest_structure(&manifest).unwrap_err().code,
        PluginErrorCode::UnsupportedApi
    );
}

#[test]
fn signatures_are_parseable_declarations_not_a_trust_or_activation_decision() {
    let mut value = frontend();
    value["signature"] = json!("opaque unverified signature");
    value["author"] = json!("Author");
    let encoded = serde_json::to_value(parsed(&value).unwrap()).unwrap();
    assert_eq!(encoded["signature"], "opaque unverified signature");
    assert_eq!(encoded["author"], "Author");
}

#[test]
fn structural_decode_errors_do_not_dump_source_text_into_error_messages() {
    let text = r#"{"id":"sample","name":"secret-document-content","version":"1.0.0","api":"2","appVersion":"*","conflicts":[],"entrypoints":{"backend":{"runtime":"secret-runtime-argument","entry":"x"}}}"#;
    let error = decode_manifest(text).unwrap_err();
    assert_eq!(error.code, PluginErrorCode::InvalidManifest);
    assert_eq!(error.plugin_id.as_deref(), Some("sample"));
    assert!(!error.message.contains("secret-document-content"));
    assert!(!error.message.contains("secret-runtime-argument"));
}
