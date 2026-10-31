//! Kaleid 无头命令行。
//!
//! 存在的意义不只是"方便"：它是**降级矩阵与启动参数的验收工具**。
//! 不启动界面也能验证「收录文件夹 → 扫描版本 → 选账号 → 生成启动命令行」整条链路。

use std::path::{Path, PathBuf};

use kaleid_api::{
    AccountServiceApi, IsolationMode, MsaAuthApi, RootRegistryService, SettingsServiceApi,
    VersionStoreService,
};
use kaleid_core_plugins::{
    all_install_sources, describe_plugins, minimal_preset, register_all, standard_preset, Accounts,
    LaunchPlanner, RootRegistry, Settings, VersionStore,
};
use kaleid_kernel::Kernel;

const USAGE: &str = r#"kaleid —— Kaleid 启动器的无头命令行

用法：
  kaleid [--data-dir <DIR>] [--minimal] <命令> [参数...]

命令：
  roots list                        列出已收录的文件夹
  roots add <路径> [--alias <别名>] [--force]
                                    收录一个文件夹（路径可以是 .minecraft 本身、
                                    它的父目录、空目录；--force 接受普通目录）
  roots remove <id|别名>            移出列表（不删磁盘）
  roots isolation <id|别名> <inherit|on|off>
  roots select <id|别名>            设为当前文件夹
  versions [<id|别名>]              列出该文件夹下的版本
  plan <版本> [--root <id|别名>] [--account <名字>]
                                    打印一次启动的完整命令行
  launch <版本> [--root <id|别名>] [--account <名字>] [--cancel-after <毫秒>]
                                    真的启动游戏，按五步打印进度（检查 / 补全 /
                                    解压 / 启动 / 等窗口），窗口出来后一直等到退出
  accounts list | off <名字> | use <id>
  settings                          打印全局设置
  plugins                           打印插件与状态，并列出已安装的第三方清单
  plugins install <路径> [--allow-unsigned]
                                    安装一份声明式清单（目录或 manifest.json）。
                                    默认要求签名；本机构建没有 ed25519 后端时，
                                    用 --allow-unsigned 安装自己确认过来源的清单
  plugins remove <id>               卸载第三方清单（不影响内置插件）
  sources                           打印可用的下载源

环境变量：
  KALEID_DATA_DIR   覆盖数据目录（默认 %LOCALAPPDATA%\\Kaleid）
"#;

fn main() {
    let code = run();
    std::process::exit(code);
}

struct Args {
    data_dir: Option<PathBuf>,
    minimal: bool,
    rest: Vec<String>,
}

fn parse_args() -> Args {
    let mut data_dir = None;
    let mut minimal = false;
    let mut rest = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--data-dir" => data_dir = it.next().map(PathBuf::from),
            "--minimal" => minimal = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => rest.push(arg),
        }
    }
    Args {
        data_dir,
        minimal,
        rest,
    }
}

fn run() -> i32 {
    let args = parse_args();
    let data_root = args
        .data_dir
        .clone()
        .unwrap_or_else(kaleid_core_plugins::data_root);

    let kernel = Kernel::new();
    register_all(&kernel, &data_root);
    // 第三方清单的配置类动作要先落到规格上，再装载内核。
    let manifests = kaleid_core_plugins::load_ready_manifests(&data_root);
    let mut specs = if args.minimal {
        minimal_preset()
    } else {
        standard_preset()
    };
    if let Err(e) = kaleid_core_plugins::apply_to_specs(&manifests, &mut specs) {
        // 清单坏了只跳过它，不拖死整个启动：用户还得能进 `plugins remove`。
        eprintln!("清单未生效：{e}");
    }
    if let Err(e) = kernel.load(&specs) {
        eprintln!("插件装载失败：{e}");
        return 2;
    }
    // 服务类动作（加下载源）在装载之后注册。
    for manifest in &manifests {
        if let Some((owner, sources)) = kaleid_core_plugins::manifest_source_table(manifest) {
            kernel.context().provide(
                "install-sources",
                kaleid_core_plugins::InstallSourceTable { owner, sources },
                0,
            );
        }
    }

    match dispatch(&kernel, &args.rest, &data_root) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn dispatch(kernel: &Kernel, rest: &[String], data_root: &Path) -> Result<(), String> {
    let ctx = kernel.context();
    let cmd = rest.first().map(|s| s.as_str()).unwrap_or("");
    let roots = ctx
        .service::<RootRegistry>()
        .ok_or_else(|| "文件夹服务不可用（core-lists 未启用？）".to_string())?;

    match cmd {
        "" | "help" => {
            println!("{USAGE}");
            Ok(())
        }
        "roots" => match rest.get(1).map(|s| s.as_str()) {
            Some("list") | None => {
                let list = roots.list().map_err(|e| e.to_string())?;
                if list.is_empty() {
                    println!("（还没有收录任何文件夹）");
                }
                let selected = roots.selected().ok().flatten().map(|r| r.id);
                for root in list {
                    let mark = if Some(&root.id) == selected.as_ref() {
                        "*"
                    } else {
                        " "
                    };
                    println!(
                        "{mark} {}  [{}]  {}  isolation={}",
                        root.alias,
                        root.id,
                        root.path,
                        root.isolation.label_zh()
                    );
                }
                Ok(())
            }
            Some("add") => {
                let path = rest.get(2).ok_or("缺少路径")?;
                let mut alias = None;
                let mut force = false;
                let mut i = 3;
                while i < rest.len() {
                    match rest[i].as_str() {
                        "--alias" => {
                            alias = rest.get(i + 1).map(|s| s.as_str());
                            i += 2;
                        }
                        "--force" => {
                            force = true;
                            i += 1;
                        }
                        other => return Err(format!("未知参数 `{other}`")),
                    }
                }
                let root = roots
                    .add(Path::new(path), alias, force)
                    .map_err(|e| e.to_string())?;
                roots.select(&root.id).map_err(|e| e.to_string())?;
                println!("已收录 {} -> {}", root.alias, root.path);
                Ok(())
            }
            Some("remove") => {
                let key = rest.get(2).ok_or("缺少 id 或别名")?;
                let root = find_root(&roots, key)?;
                roots.remove(&root.id).map_err(|e| e.to_string())?;
                println!("已移出列表（磁盘文件未改动）：{}", root.path);
                Ok(())
            }
            Some("isolation") => {
                let key = rest.get(2).ok_or("缺少 id 或别名")?;
                let mode = match rest.get(3).map(|s| s.as_str()) {
                    Some("inherit") => IsolationMode::Inherit,
                    Some("on") => IsolationMode::On,
                    Some("off") => IsolationMode::Off,
                    _ => return Err("隔离模式只能是 inherit / on / off".into()),
                };
                let root = find_root(&roots, key)?;
                let updated = roots
                    .set_isolation(&root.id, mode)
                    .map_err(|e| e.to_string())?;
                println!("{} 的版本隔离 = {}", updated.alias, mode.label_zh());
                Ok(())
            }
            Some("select") => {
                let key = rest.get(2).ok_or("缺少 id 或别名")?;
                let root = find_root(&roots, key)?;
                roots.select(&root.id).map_err(|e| e.to_string())?;
                println!("当前文件夹：{}", root.alias);
                Ok(())
            }
            Some(other) => Err(format!("未知子命令 roots {other}")),
        },
        "versions" => {
            let store = ctx.service::<VersionStore>().ok_or("版本扫描服务不可用")?;
            let root = match rest.get(1) {
                Some(key) => find_root(&roots, key)?,
                None => roots
                    .selected()
                    .map_err(|e| e.to_string())?
                    .ok_or("还没有选择文件夹")?,
            };
            let list = store
                .scan(Path::new(&root.path))
                .map_err(|e| e.to_string())?;
            if list.is_empty() {
                println!("{} 下没有版本", root.alias);
            }
            for v in list {
                let status = match v.readiness {
                    kaleid_api::Readiness::Ready => "就绪".to_string(),
                    kaleid_api::Readiness::Incomplete { missing } => format!("缺 {missing} 文件"),
                    kaleid_api::Readiness::Unknown => "未知".to_string(),
                };
                println!(
                    "{:<24} {:<12} {:<10} {}",
                    v.id,
                    v.loader.label_zh(),
                    v.loader_version.unwrap_or_default(),
                    status
                );
            }
            Ok(())
        }
        "plan" => {
            let version = rest.get(1).ok_or("缺少版本 id")?;
            let planner = ctx.service::<LaunchPlanner>().ok_or("启动服务不可用")?;
            let accounts = ctx.service::<Accounts>().ok_or("账号服务不可用")?;

            let mut root_key: Option<&str> = None;
            let mut account_name: Option<&str> = None;
            let mut i = 2;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--root" => {
                        root_key = rest.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    "--account" => {
                        account_name = rest.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    other => return Err(format!("未知参数 `{other}`")),
                }
            }
            let root = match root_key {
                Some(key) => find_root(&roots, key)?,
                None => roots
                    .selected()
                    .map_err(|e| e.to_string())?
                    .ok_or("还没有选择文件夹")?,
            };
            let account = match account_name {
                Some(name) => accounts
                    .apply_offline_name(name)
                    .map_err(|e| e.to_string())?,
                None => accounts
                    .active()
                    .map_err(|e| e.to_string())?
                    .ok_or("还没有账号，用 `kaleid accounts off <名字>` 建一个")?,
            };
            let plan = planner
                .plan_for_root(&root, version, &account)
                .map_err(|e| e.to_string())?;
            println!("{}", plan.command_line());
            Ok(())
        }
        "launch" => {
            let version = rest.get(1).ok_or("缺少版本 id")?;
            let planner = ctx.service::<LaunchPlanner>().ok_or("启动服务不可用")?;
            let accounts = ctx.service::<Accounts>().ok_or("账号服务不可用")?;
            let game = ctx
                .service::<kaleid_core_plugins::GameProcessManager>()
                .ok_or("启动插件不可用")?;

            let mut root_key: Option<&str> = None;
            let mut account_name: Option<&str> = None;
            let mut cancel_after: Option<u64> = None;
            let mut i = 2;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--root" => {
                        root_key = rest.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    "--account" => {
                        account_name = rest.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    "--cancel-after" => {
                        cancel_after = rest.get(i + 1).and_then(|s| s.parse().ok());
                        i += 2;
                    }
                    other => return Err(format!("未知参数 `{other}`")),
                }
            }
            let root = match root_key {
                Some(key) => find_root(&roots, key)?,
                None => roots
                    .selected()
                    .map_err(|e| e.to_string())?
                    .ok_or("还没有选择文件夹")?,
            };
            let account = match account_name {
                Some(name) => accounts
                    .apply_offline_name(name)
                    .map_err(|e| e.to_string())?,
                None => accounts
                    .active()
                    .map_err(|e| e.to_string())?
                    .ok_or("还没有账号，用 `kaleid accounts off <名字>` 建一个")?,
            };
            let plan = planner
                .plan_for_root(&root, version, &account)
                .map_err(|e| e.to_string())?;
            let token = ctx
                .service::<kaleid_core_plugins::MsaAuth>()
                .and_then(|msa| msa.access_token(&account.id).ok().flatten());
            let mods = std::fs::read_dir(Path::new(&plan.game_dir).join("mods"))
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|e| e.path().is_file())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            let started = game.start(plan, token, mods).map_err(|e| e.to_string())?;
            if let Some(ms) = cancel_after {
                let game_ref = game.clone();
                let session = started.session_id;
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    if let Err(e) = game_ref.cancel(session) {
                        eprintln!("取消不了：{e}");
                    }
                });
            }
            // 按五步打印进度：只在文案变化时打一行，别把终端刷满。
            let mut last = String::new();
            let mut seen_running = false;
            loop {
                let state = game
                    .states()
                    .into_iter()
                    .find(|s| s.session_id == started.session_id)
                    .ok_or("会话已消失")?;
                let line = format!("{:>3.0}% {}", state.percent * 100.0, state.stage_label);
                if line != last {
                    println!("{line}");
                    last = line;
                }
                match state.phase {
                    kaleid_api::LaunchPhase::Running if !seen_running => {
                        seen_running = true;
                        println!(
                            "✅ 游戏已启动（pid {}，用时 {} ms）",
                            state.pid.unwrap_or_default(),
                            state.elapsed_ms
                        );
                    }
                    kaleid_api::LaunchPhase::Exited => {
                        println!("游戏已退出（退出码 {:?}）", state.exit_code);
                        return Ok(());
                    }
                    kaleid_api::LaunchPhase::Failed => {
                        let reason = state
                            .error
                            .clone()
                            .unwrap_or_else(|| {
                                if state.crashed {
                                    "游戏异常退出，可导出排障包看详情".to_string()
                                } else {
                                    "启动失败".to_string()
                                }
                            });
                        println!("❌ {reason}");
                        for step in &state.steps {
                            if step.status == kaleid_api::PhaseStatus::Failed {
                                println!("   失败步骤：{} · {}", step.label, step.detail);
                            }
                        }
                        return Err("启动失败".to_string());
                    }
                    kaleid_api::LaunchPhase::Cancelled => {
                        println!("已取消启动");
                        return Ok(());
                    }
                    _ => {}
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        "accounts" => {
            let accounts = ctx.service::<Accounts>().ok_or("账号服务不可用")?;            match rest.get(1).map(|s| s.as_str()) {
                Some("off") => {
                    let name = rest.get(2).ok_or("缺少名字")?;
                    let acc = accounts
                        .apply_offline_name(name)
                        .map_err(|e| e.to_string())?;
                    println!("离线账号已生效：{} ({})", acc.name, acc.uuid);
                    Ok(())
                }
                Some("use") => {
                    let id = rest.get(2).ok_or("缺少账号 id")?;
                    let acc = accounts.set_active(id).map_err(|e| e.to_string())?;
                    println!("当前账号：{}", acc.name);
                    Ok(())
                }
                _ => {
                    let active = accounts.active().map_err(|e| e.to_string())?;
                    for acc in accounts.list().map_err(|e| e.to_string())? {
                        let mark = if active.as_ref().map(|a| &a.id) == Some(&acc.id) {
                            "*"
                        } else {
                            " "
                        };
                        println!("{mark} {}  {}  {}", acc.name, acc.kind.label_zh(), acc.id);
                    }
                    Ok(())
                }
            }
        }
        "settings" => {
            let settings = ctx.service::<Settings>().ok_or("设置服务不可用")?;
            let global = settings.global().map_err(|e| e.to_string())?;
            println!("{}", serde_json::to_string_pretty(&global).unwrap());
            Ok(())
        }
        "plugins" => {
            match rest.get(1).map(|s| s.as_str()) {
                Some("install") => {
                    let Some(source) = rest.get(2) else {
                        return Err(
                            "用法：kaleid plugins install <manifest.json 或目录> [--allow-unsigned]"
                                .into(),
                        );
                    };
                    let allow_unsigned = rest.iter().any(|a| a == "--allow-unsigned");
                    let reserved: Vec<String> = describe_plugins(kernel)
                        .into_iter()
                        .map(|plugin| plugin.id)
                        .collect();
                    let (manifest, path) = kaleid_core_plugins::install_manifest(
                        data_root,
                        Path::new(source),
                        allow_unsigned,
                        &reserved,
                    )
                    .map_err(|e| e)?;
                    println!(
                        "已安装 {} {} → {}",
                        manifest.name,
                        manifest.version,
                        path.display()
                    );
                    if !allow_unsigned {
                        println!("（已校验签名）");
                    }
                    println!("下次启动生效。");
                    return Ok(());
                }
                Some("remove") => {
                    let Some(id) = rest.get(2) else {
                        return Err("用法：kaleid plugins remove <id>".into());
                    };
                    kaleid_core_plugins::remove_manifest(data_root, id).map_err(|e| e)?;
                    println!("已移除 `{id}`。下次启动生效。");
                    return Ok(());
                }
                Some(other) => {
                    return Err(format!("未知的 plugins 子命令 `{other}`（install / remove）"))
                }
                None => {}
            }
            for p in describe_plugins(kernel) {
                println!(
                    "{} {:<18} {:<12} {}",
                    if p.enabled { "●" } else { "○" },
                    p.id,
                    format!("{:?}", p.state),
                    p.description
                );
            }
            let installed = kaleid_core_plugins::list_manifests(data_root);
            if installed.is_empty() {
                println!("\n没有第三方清单（<数据目录>/plugins/<id>/manifest.json）");
            } else {
                println!("\n第三方清单：");
                for m in installed {
                    match &m.error {
                        Some(error) => println!("  ✗ {:<20} {}（{error}）", m.id, m.path),
                        None => println!("  ✓ {:<20} {} {}", m.id, m.version, m.name),
                    }
                }
            }
            Ok(())
        }
        "sources" => {
            for s in all_install_sources(ctx) {
                println!(
                    "{:<10} prio={:<4} {}\n    manifest: {}\n    base:     {}",
                    s.id(),
                    s.priority(),
                    s.label(),
                    s.manifest_url(),
                    s.base_url()
                );
            }
            Ok(())
        }
        other => Err(format!("未知命令 `{other}`，试试 `kaleid help`")),
    }
}

fn find_root(roots: &std::sync::Arc<RootRegistry>, key: &str) -> Result<kaleid_api::Root, String> {
    let list = roots.list().map_err(|e| e.to_string())?;
    list.into_iter()
        .find(|r| r.id == key || r.alias == key || r.path == key)
        .ok_or_else(|| format!("找不到文件夹 `{key}`"))
}

#[allow(dead_code)]
fn _unused(_: &dyn SettingsServiceApi) {}


