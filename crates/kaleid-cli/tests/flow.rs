//! 端到端（离线，不联网）：从"收录文件夹"走到"生成启动命令行"。
//!
//! 这个测试不碰网络、不启动游戏，只验证内核 + 内置插件拼起来是否真的可用，
//! 以及降级矩阵（停用版本管理）是否仍然能启动。

use std::path::{Path, PathBuf};

use serde_json::json;

use kaleid_api::{
    validate_offline_name, AccountKind, AccountServiceApi, LoaderKind, Readiness,
    RootRegistryService, SettingsServiceApi, VersionStoreService,
};
use kaleid_core_plugins::{
    all_install_sources, describe_plugins, minimal_preset, register_all, standard_preset, Accounts,
    LaunchPlanner, RootRegistry, Settings, VersionStore,
};
use kaleid_kernel::Kernel;

struct Env {
    _dir: tempfile::TempDir,
    data: PathBuf,
    mc: PathBuf,
}

/// 造一个像模像样的 `.minecraft`：一个原版 + 一个 Fabric（继承原版）。
fn make_env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("kaleid-data");
    let mc = dir.path().join(".minecraft");
    std::fs::create_dir_all(mc.join("versions/1.21.4")).unwrap();
    std::fs::create_dir_all(mc.join("versions/1.21.4-fabric")).unwrap();
    std::fs::create_dir_all(mc.join("libraries/net/fabricmc/fabric-loader/0.16.9")).unwrap();
    std::fs::create_dir_all(mc.join("libraries/com/mojang/logging/1.21.4")).unwrap();
    std::fs::create_dir_all(mc.join("assets")).unwrap();
    // readiness 会把「资源索引缺失」也算进 Incomplete，所以这里要造一份索引；
    // objects 留空表示不需要额外的资源文件。
    std::fs::create_dir_all(mc.join("assets/indexes")).unwrap();
    std::fs::write(
        mc.join("assets/indexes/17.json"),
        serde_json::to_string(&json!({ "objects": {} })).unwrap(),
    )
    .unwrap();

    let vanilla = json!({
        "id": "1.21.4",
        "type": "release",
        "mainClass": "net.minecraft.client.main.Main",
        "assets": "17",
        "assetIndex": { "id": "17" },
        "javaVersion": { "majorVersion": 21 },
        "downloads": { "client": { "url": "https://example/client.jar" } },
        "libraries": [
            { "name": "com.mojang:logging:1.21.4",
              "downloads": { "artifact": { "path": "com/mojang/logging/1.21.4/logging-1.21.4.jar" } } }
        ],
        "arguments": {
            "jvm": ["-cp", "${classpath}"],
            "game": ["--username", "${auth_player_name}", "--uuid", "${auth_uuid}",
                     "--gameDir", "${game_directory}", "--assetsDir", "${assets_root}"]
        }
    });
    std::fs::write(
        mc.join("versions/1.21.4/1.21.4.json"),
        serde_json::to_string_pretty(&vanilla).unwrap(),
    )
    .unwrap();
    std::fs::write(mc.join("versions/1.21.4/1.21.4.jar"), b"jar").unwrap();
    std::fs::write(
        mc.join("libraries/com/mojang/logging/1.21.4/logging-1.21.4.jar"),
        b"lib",
    )
    .unwrap();

    let fabric = json!({
        "id": "1.21.4-fabric",
        "inheritsFrom": "1.21.4",
        "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
        "libraries": [
            { "name": "net.fabricmc:fabric-loader:0.16.9",
              "downloads": { "artifact": { "path": "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar" } } }
        ]
    });
    std::fs::write(
        mc.join("versions/1.21.4-fabric/1.21.4-fabric.json"),
        serde_json::to_string_pretty(&fabric).unwrap(),
    )
    .unwrap();
    std::fs::write(
        mc.join("libraries/net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"),
        b"lib",
    )
    .unwrap();

    Env {
        _dir: dir,
        data,
        mc,
    }
}

fn boot(env: &Env, minimal: bool) -> Kernel {
    let kernel = Kernel::new();
    register_all(&kernel, &env.data);
    let specs = if minimal {
        minimal_preset()
    } else {
        standard_preset()
    };
    kernel.load(&specs).expect("插件应能装载");
    kernel
}

#[test]
fn full_offline_flow_from_folder_to_command_line() {
    let env = make_env();
    let kernel = boot(&env, false);
    let ctx = kernel.context();

    // 1) 收录文件夹
    let roots = ctx.service::<RootRegistry>().unwrap();
    let root = roots.add(&env.mc, Some("默认（Roaming）"), false).unwrap();
    roots.select(&root.id).unwrap();
    assert_eq!(roots.selected().unwrap().unwrap().id, root.id);

    // 2) 扫描版本并分类
    let store = ctx.service::<VersionStore>().unwrap();
    let versions = store.scan(Path::new(&root.path)).unwrap();
    assert_eq!(versions.len(), 2);
    let fabric = versions.iter().find(|v| v.id == "1.21.4-fabric").unwrap();
    assert_eq!(fabric.loader, LoaderKind::Fabric);
    assert_eq!(fabric.loader_version.as_deref(), Some("0.16.9"));
    assert_eq!(fabric.readiness, Readiness::Ready);
    let vanilla = versions.iter().find(|v| v.id == "1.21.4").unwrap();
    assert_eq!(vanilla.loader, LoaderKind::Vanilla);
    assert_eq!(vanilla.java_major, Some(21));

    // 3) 离线账号：输入即生效
    let accounts = ctx.service::<Accounts>().unwrap();
    let account = accounts.apply_offline_name("Player_123").unwrap();
    assert_eq!(account.kind, AccountKind::Offline);
    assert_eq!(accounts.active().unwrap().unwrap().uuid, account.uuid);
    assert!(accounts.apply_offline_name("坏名字!").is_err());

    // 4) 版本级设置覆盖全局
    let settings = ctx.service::<Settings>().unwrap();
    settings.patch_global(&json!({"memoryMb": 4096})).unwrap();
    settings
        .patch_version(
            &root.id,
            "1.21.4-fabric",
            &json!({"memoryMb": 2048, "jvmArgs": ["-XX:+UseG1GC"]}),
        )
        .unwrap();

    // 5) 生成启动命令行（隔离默认开启）
    let planner = ctx.service::<LaunchPlanner>().unwrap();
    let plan = planner
        .plan_for_root(&root, "1.21.4-fabric", &account)
        .unwrap();
    assert!(plan.isolated, "版本隔离内置默认必须是开启");
    assert!(
        plan.game_dir.ends_with("versions\\1.21.4-fabric")
            || plan.game_dir.ends_with("versions/1.21.4-fabric")
    );
    assert_eq!(plan.username, "Player_123");
    assert_eq!(
        plan.main_class,
        "net.fabricmc.loader.impl.launch.knot.KnotClient"
    );
    assert!(plan.extra_jvm_args.contains(&"-Xmx2048M".to_string()));
    assert!(plan.extra_jvm_args.contains(&"-XX:+UseG1GC".to_string()));
    // classpath 里既有 fabric-loader 也有父版本的客户端 jar
    assert!(plan
        .classpath
        .iter()
        .any(|p| p.ends_with("fabric-loader-0.16.9.jar")));
    assert!(plan.classpath.iter().any(|p| p.ends_with("1.21.4.jar")));
    let cmd = plan.command_line();
    assert!(cmd.contains("Player_123"));

    // 6) 关掉隔离 → gameDir 回到根目录
    roots
        .set_isolation(&root.id, kaleid_api::IsolationMode::Off)
        .unwrap();
    let root_off = roots.get(&root.id).unwrap();
    let plan_off = planner
        .plan_for_root(&root_off, "1.21.4-fabric", &account)
        .unwrap();
    assert!(!plan_off.isolated);
    assert_eq!(Path::new(&plan_off.game_dir), env.mc);
}

#[test]
fn offline_name_rules_match_api_contract() {
    // CLI 与界面共用同一套规则，避免两边漂移
    for ok in ["Player_123", "a", "abcdefghijklmnop", "小明", "中文名ABC_1"] {
        assert!(validate_offline_name(ok).is_ok(), "{ok} 应合法");
    }
    for bad in [
        "",
        "名字 空格",
        "bad name",
        "bad-name",
        "abcdefghijklmnopq",
        "ひらがな",
        "名字!",
    ] {
        assert!(validate_offline_name(bad).is_err(), "{bad} 应非法");
    }
}

#[test]
fn degrading_version_manager_keeps_lists_launch_and_settings() {
    let env = make_env();
    let kernel = boot(&env, true); // minimal preset：version-manager 被停用
    let ctx = kernel.context();

    // 版本管理的贡献（下载源）消失
    assert!(
        all_install_sources(ctx).is_empty(),
        "停用版本管理后不应还有下载源"
    );
    let descriptors = describe_plugins(&kernel);
    let vm = descriptors
        .iter()
        .find(|p| p.id == "version-manager")
        .expect("插件管理页仍应列出该插件");
    assert!(!vm.enabled);

    // 但文件夹、版本、启动、设置都还在
    let roots = ctx.service::<RootRegistry>().expect("文件夹列表必须仍在");
    let root = roots.add(&env.mc, None, false).unwrap();
    let store = ctx.service::<VersionStore>().expect("版本扫描必须仍在");
    assert_eq!(store.scan(Path::new(&root.path)).unwrap().len(), 2);
    let settings = ctx.service::<Settings>().expect("设置必须仍在");
    assert_eq!(settings.global().unwrap().memory_mb, 4096);
    let planner = ctx.service::<LaunchPlanner>().expect("启动必须仍可用");
    let accounts = ctx.service::<Accounts>().unwrap();
    let account = accounts.apply_offline_name("Player_123").unwrap();
    let plan = planner.plan_for_root(&root, "1.21.4", &account).unwrap();
    assert_eq!(plan.username, "Player_123");
}

#[test]
fn standard_preset_exposes_both_download_sources() {
    let env = make_env();
    let kernel = boot(&env, false);
    let sources = all_install_sources(kernel.context());
    let ids: Vec<&str> = sources.iter().map(|s| s.id()).collect();
    assert!(ids.contains(&"mojang"), "应包含官方源：{ids:?}");
    assert!(ids.contains(&"bmclapi"), "应包含 BMCLAPI：{ids:?}");
    // BMCLAPI 优先级更高，排在前面（用于自动测速的候选顺序）
    assert_eq!(sources[0].id(), "bmclapi");
}

#[test]
fn disabling_a_plugin_rolls_back_its_service() {
    let env = make_env();
    let kernel = boot(&env, false);
    let ctx = kernel.context();
    assert!(!all_install_sources(ctx).is_empty());

    // 用可选的 version-manager 验证回滚：它注册的下载源要一起消失。
    kernel.disable("version-manager").unwrap();
    assert!(
        all_install_sources(ctx).is_empty(),
        "停用 version-manager 后安装源必须被回收"
    );
    // 其它服务不受影响
    assert!(ctx.service::<Settings>().is_some());

    kernel.enable("version-manager").unwrap();
    assert!(!all_install_sources(ctx).is_empty(), "重新启用后应恢复");
}

#[test]
fn required_plugins_cannot_be_disabled() {
    let env = make_env();
    let kernel = boot(&env, false);
    let ctx = kernel.context();
    assert!(ctx.service::<RootRegistry>().is_some());

    // Java 运行时与本地内容都是地基：停掉之后要么启动不了，要么版本页整页空白。
    for id in ["core-lists", "java-runtime", "game-content"] {
        let err = kernel.disable(id).unwrap_err().to_string();
        assert!(err.contains("必需插件"), "{id} 应拒绝停用，实际：{err}");
    }
    assert!(
        ctx.service::<RootRegistry>().is_some(),
        "被拒绝的停用不能有任何副作用"
    );

    // 描述里也要带上 required，界面才能把开关置灰。
    let required: Vec<String> = describe_plugins(&kernel)
        .into_iter()
        .filter(|p| p.required)
        .map(|p| p.id)
        .collect();
    assert!(required.contains(&"java-runtime".to_string()));
    assert!(required.contains(&"game-content".to_string()));
    assert!(!required.contains(&"version-manager".to_string()));
}

/// CLI 安装、列出、卸载第三方清单的完整链路（进程内，不联网）。
///
/// 这条用例守着文档里写的那三条命令：装上之后 `sources` 能读到新源，
/// 卸载之后源跟着消失，内置源自始至终都还在。
#[test]
fn cli_installs_and_removes_a_manifest_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("kaleid-data");
    std::fs::create_dir_all(&data).unwrap();
    let manifest_path = dir.path().join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string(&json!({
            "id": "example-mirror",
            "name": "校园镜像源",
            "version": "1.0.0",
            "api": "1",
            "steps": [{
                "op": "provide-source",
                "id": "campus-mirror",
                "label": "校园网镜像",
                "priority": 20,
                "manifestUrl": "https://mirror.example.edu/minecraft/version_manifest_v2.json",
                "baseUrl": "https://mirror.example.edu/minecraft"
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let run = |args: &[&str]| -> (bool, String) {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_kaleid"))
            .arg("--data-dir")
            .arg(&data)
            .args(args)
            .output()
            .expect("运行 kaleid");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    };

    // 无签名且没有 --allow-unsigned：必须拒绝。
    let (ok, text) = run(&["plugins", "install", &manifest_path.to_string_lossy()]);
    assert!(!ok, "默认应当拒绝无签名清单：{text}");

    let (ok, text) = run(&[
        "plugins",
        "install",
        &manifest_path.to_string_lossy(),
        "--allow-unsigned",
    ]);
    assert!(ok, "显式允许后应当装上：{text}");
    assert!(data.join("plugins/example-mirror/manifest.json").is_file());

    let (ok, text) = run(&["plugins"]);
    assert!(ok);
    assert!(text.contains("example-mirror"), "{text}");

    let (ok, text) = run(&["sources"]);
    assert!(ok);
    assert!(text.contains("campus-mirror"), "新源应当出现在 sources 里：{text}");
    assert!(text.contains("mojang"), "内置源不能被顶掉：{text}");

    let (ok, text) = run(&["plugins", "remove", "example-mirror"]);
    assert!(ok, "卸载应当成功：{text}");

    let (ok, text) = run(&["sources"]);
    assert!(ok);
    assert!(!text.contains("campus-mirror"), "卸载后源应当消失：{text}");
    assert!(text.contains("mojang"), "内置源不受影响：{text}");

    let (text, records) = logger_cli_records(&data);
    let installs: Vec<_> = records.iter().filter(|r| r["event"] == "command.result" && r["fields"]["command"] == "plugins_install").collect();
    assert_eq!(installs.len(), 2, "{text}");
    assert!(installs.iter().any(|r| r["fields"]["outcome"] == "failed"));
    assert!(installs.iter().any(|r| r["fields"]["outcome"] == "succeeded"));
    assert!(records.iter().any(|r| r["event"] == "command.result" && r["fields"]["command"] == "plugins_remove" && r["fields"]["outcome"] == "succeeded"));
    assert!(!text.contains(&manifest_path.to_string_lossy().to_string()));
}

fn logger_cli_records(data: &Path) -> (String, Vec<serde_json::Value>) {
    let mut text = String::new();
    let runs = std::fs::read_dir(data.join("logs/launcher")).expect("CLI must persist a run before process exit");
    for run in runs.flatten() {
        if !run.path().is_dir() { continue; }
        for entry in std::fs::read_dir(run.path()).unwrap().flatten() {
            if entry.path().is_file() { text.push_str(&std::fs::read_to_string(entry.path()).unwrap()); }
        }
    }
    let records = text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    (text, records)
}

// A pre-dispatch boot failure must still drain the boot/exit record before code 2.
#[test]
fn logger_cli_boot_failure_flushes_before_exit() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let plugin = data.join("plugins/bad-config");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("manifest.json"), json!({
        "id":"bad-config", "name":"bad", "version":"1", "api":"1",
        "steps":[{"op":"update-config", "target":"version-manager", "patch":{"enableMojang":"PRIVATE_CLI_BOOT_3A"}}]
    }).to_string()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kaleid"))
        .arg("--data-dir").arg(&data).args(["roots", "list"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let (text, records) = logger_cli_records(&data);
    assert!(records.iter().any(|r| r["event"] == "startup.result" && r["fields"]["outcome"] == "failed"));
    assert!(records.iter().any(|r| r["event"] == "cli.exit" && r["fields"]["exit_code"] == 2));
    assert_eq!(records.iter().filter(|r| r["event"] == "writer.shutdown").count(), 1);
    assert!(!text.contains("PRIVATE_CLI_BOOT_3A"));
}

// A real process::exit must not lose queued invocation/exit records or persist argv/errors.
#[test]
fn logger_cli_actual_exit_persists_safe_success_and_failure_results() {
    for minimal in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let run = |args: &[&str]| {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_kaleid"));
            command.arg("--data-dir").arg(&data);
            if minimal { command.arg("--minimal"); }
            command.args(args).output().unwrap()
        };
        assert!(run(&["roots", "list"]).status.success());
        let sentinels = ["SENTINEL_TOKEN_CLI_3A", "DEVICECODE_CLI_3A", "-DprivateJvm=CLI3A", "https://user:credentialCLI3A@example.invalid/private", "SecretCliUser42"];
        assert!(!run(&["roots", "remove", sentinels[4]]).status.success());
        assert!(!run(&sentinels).status.success());
        let (text, records) = logger_cli_records(&data);
        assert_eq!(records.iter().filter(|r| r["event"] == "startup.result" && r["fields"]["outcome"] == "succeeded").count(), 3);
        for (command, outcome) in [("roots_list", "succeeded"), ("roots_remove", "failed"), ("unknown", "failed")] {
            let results: Vec<_> = records.iter().filter(|r| r["event"] == "command.result" && r["fields"]["command"] == command).collect();
            assert_eq!(results.len(), 1, "{command}: {text}");
            assert_eq!(results[0]["fields"]["outcome"], outcome);
            assert!(results[0]["operation_id"].is_string());
            assert!(results[0]["fields"]["duration_ms"].is_u64());
        }
        assert_eq!(records.iter().filter(|r| r["event"] == "cli.exit").count(), 3);
        assert_eq!(records.iter().filter(|r| r["event"] == "writer.shutdown").count(), 3);
        for sentinel in sentinels { assert!(!text.contains(sentinel), "leaked {sentinel}"); }
    }
}
