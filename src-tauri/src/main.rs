//! Kaleid 的桌面外壳。
//!
//! 这一层**刻意薄到没有业务**：只做一个 `kaleid_invoke(cmd, args)` 转发。
//! 业务全在 `kaleid-host` 的命令表里，所以换外壳（Tauri / 纯 IPC / CLI）不影响逻辑。
//!
//! 窗口在 Rust 侧创建而**不是**写在 `tauri.conf.json` 里，唯一原因是必须用
//! `initialization_script` 在页面脚本之前注入 `window.__KALEID__`：
//! 界面用这个对象说话，缺失时它会退回浏览器 mock（那样就等于真机跑演示数据）。
//! 同时坚持 `withGlobalTauri: false`，界面拿不到原生 API，只能走我们的命令表。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeSet;
use std::sync::Arc;

use base64::Engine;
use serde_json::{json, Value};
use tauri::{DragDropEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use kaleid_host::{bootstrap_snapshot, Host};

struct AppState {
    host: Arc<Host>,
}

/// 界面统一入口：`invoke('kaleid_invoke', { cmd, args })`
#[tauri::command]
async fn kaleid_invoke(
    state: State<'_, AppState>,
    cmd: String,
    args: Option<Value>,
) -> Result<Value, String> {
    let host = state.host.clone();
    tauri::async_runtime::spawn_blocking(move || host.dispatch(&cmd, args.unwrap_or(Value::Null)))
        .await
        .map_err(|e| format!("后台任务失败：{e}"))?
}

/// 原生文件夹选择器。只返回用户真正选中的目录。
#[tauri::command]
async fn kaleid_pick_folder() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .pick_folder()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| format!("文件夹选择失败：{e}"))
}

/// 原生字体文件选择器。返回路径；读取走 `kaleid_font_read`。
#[tauri::command]
async fn kaleid_pick_font() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .add_filter("字体文件", &["ttf", "otf", "ttc"])
            .pick_file()
            .map(|p| p.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| format!("字体选择失败：{e}"))
}

/// 原生多选文件对话框（导入模组/资源包用）。
///
/// `extensions` 为空表示不限类型；返回全部选中路径，用户取消就是空数组。
#[tauri::command]
async fn kaleid_pick_files(extensions: Option<Vec<String>>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new();
        let exts: Vec<String> = extensions
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        if !exts.is_empty() {
            let refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
            dialog = dialog.add_filter("支持的文件", &refs);
        }
        dialog
            .pick_files()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| format!("文件选择失败：{e}"))
}

/// 列出系统已安装字体族。
#[tauri::command]
async fn kaleid_fonts_list() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut families = BTreeSet::new();
        for face in db.faces() {
            for (family, _) in &face.families {
                let family = family.trim();
                if !family.is_empty() {
                    families.insert(family.to_string());
                }
            }
        }
        families.into_iter().collect::<Vec<_>>()
    })
    .await
    .map_err(|e| format!("读取系统字体失败：{e}"))
}

/// 读取一个字体文件并转成 data URL 内容（base64）。限制 32 MiB。
#[tauri::command]
async fn kaleid_font_read(path: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = std::path::Path::new(&path);
        if !p.is_file() {
            return Err(format!("不是字体文件：{path}"));
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "ttf" | "otf" | "ttc") {
            return Err("只支持 ttf / otf / ttc 字体".into());
        }
        let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("字体文件超过 32 MiB".into());
        }
        let name = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Imported Font".into());
        Ok(json!({
            "path": path,
            "name": name,
            "fileName": p.file_name().map(|s| s.to_string_lossy().to_string()),
            "extension": ext,
            "size": bytes.len(),
            "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }))
    })
    .await
    .map_err(|e| format!("读取字体失败：{e}"))?
}

/// 开屏只要一次：文件夹、当前选中、全局设置、账号、插件、下载源。
#[tauri::command]
fn kaleid_bootstrap(state: State<'_, AppState>) -> Result<Value, String> {
    Ok(bootstrap_snapshot(state.host.kernel()))
}

/// 只允许打开真实存在的目录，不接受任意路径。
#[tauri::command]
fn kaleid_open_dir(path: String) -> Result<(), String> {
    let real = std::path::Path::new(&path);
    if !real.is_dir() {
        return Err(format!("不是目录：{path}"));
    }
    opener_open(&path)
}

#[cfg(windows)]
fn opener_open(path: &str) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn opener_open(_path: &str) -> Result<(), String> {
    Err("当前平台未实现".into())
}

/// 用系统浏览器打开外部链接（新闻条目、帮助页）。
///
/// 只放行 http/https：否则这个命令就成了"任意协议启动器"，
/// 一个坏掉的新闻链接就能拿它去调 `file:` 或自定义协议。
#[tauri::command]
fn kaleid_open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("只允许 http/https 链接：{url}"));
    }
    opener_open_url(&url)
}

#[cfg(windows)]
fn opener_open_url(url: &str) -> Result<(), String> {
    // `start "" <url>`：第一个空参数是窗口标题占位，不加的话带引号的 URL
    // 会被 cmd 当成标题，浏览器根本打不开。
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn opener_open_url(_url: &str) -> Result<(), String> {
    Err("当前平台未实现".into())
}

/// 切换外壳自己（标题栏/边框）的明暗。
///
/// 界面换主题时同步调一下：只换网页里的 CSS 变量的话，浅色界面会顶着一条
/// 黑色标题栏，看起来像没做完。只接受 dark / light，其它值直接报错。
#[tauri::command]
fn kaleid_set_theme(window: tauri::WebviewWindow, theme: String) -> Result<(), String> {
    let target = match theme.as_str() {
        "light" => tauri::Theme::Light,
        "dark" => tauri::Theme::Dark,
        other => return Err(format!("未知主题：{other}")),
    };
    window.set_theme(Some(target)).map_err(|e| e.to_string())
}

/// 启动游戏后的窗口动作。
///
/// 四个值对应设置里的「启动游戏后操作」：最小化、隐藏到托盘、退出启动器、保持不变
/// （`keep` 不会调到这里）。`restore` 是回头路：隐藏后游戏退出，窗口得自己回来——
/// 这也是「隐藏至托盘（退出游戏重新打开）」这句需求的实际含义。
///
/// 不用系统托盘图标：这个壳子平时就没有托盘，藏起来再弹回来比常驻一个图标更符合
/// 使用习惯；真需要托盘的话应该是单独一版设计，而不是顺手加进来。
#[tauri::command]
fn kaleid_after_launch(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    action: String,
) -> Result<(), String> {
    match action.as_str() {
        "minimize" => window.minimize().map_err(|e| e.to_string()),
        "hide_to_tray" => window.hide().map_err(|e| e.to_string()),
        "restore" => {
            window.show().map_err(|e| e.to_string())?;
            window.unminimize().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())
        }
        // 退出启动器：窗口销毁后进程由 Tauri 自己收尾（游戏进程不跟着死）。
        "exit" => {
            app.exit(0);
            Ok(())
        }
        other => Err(format!("未知的启动后动作：{other}")),
    }
}

/// 注入到界面里的桥。
///
/// 暴露的都是窄接口（invoke / listen / openExternal / setTheme / 选目录与文件 /\n/// 读字体 / 启动后窗口动作），且都固定走
/// UTF-8 JSON。`listen` 在没有推送需求前是空实现，但保留了正确的签名，
/// 等下载/安装插件接上事件时只需把内部换成真正的订阅。
const BRIDGE_JS: &str = r#"
(function () {
  if (window.__KALEID__) return;
  var internals = window.__TAURI_INTERNALS__;
  if (!internals) {
    // 外壳没准备好：明确报错，不要静默退回 mock（那会让人以为功能是好的）
    window.__KALEID__ = {
      invoke: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      listen: function () { return Promise.resolve(function () {}); },
      openExternal: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      setTheme: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      applyAfterLaunch: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      pickFolder: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      pickFiles: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      onFileDrop: function () { return function () {}; },
      openDir: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      pickFont: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      },
      listFonts: function () {
        return Promise.resolve([]);
      },
      readFont: function () {
        return Promise.reject(new Error('Kaleid 桥未就绪：__TAURI_INTERNALS__ 不存在'));
      }
    };
    return;
  }
  window.__KALEID__ = {
    invoke: function (cmd, args) {
      return internals.invoke('kaleid_invoke', { cmd: cmd, args: args === undefined ? null : args });
    },
    listen: function (_event, _cb) {
      return Promise.resolve(function () {});
    },
    // 外部链接必须交给系统浏览器：在 WebView 里直接导航会把界面本身带走。
    openExternal: function (url) {
      return internals.invoke('kaleid_open_url', { url: url });
    },
    // 标题栏明暗跟着界面主题走。
    setTheme: function (theme) {
      return internals.invoke('kaleid_set_theme', { theme: theme });
    },
    // 启动游戏后的窗口动作：minimize / hide_to_tray / exit / restore。
    applyAfterLaunch: function (action) {
      return internals.invoke('kaleid_after_launch', { action: action });
    },
    pickFolder: function () {
      return internals.invoke('kaleid_pick_folder', {});
    },
    openDir: function (path) {
      return internals.invoke('kaleid_open_dir', { path: path });
    },
    // 多选文件（导入模组/资源包）。extensions 为空就是"不限类型"。
    pickFiles: function (extensions) {
      return internals.invoke('kaleid_pick_files', { extensions: extensions || [] });
    },
    // 系统级拖拽：Windows 下 Tauri 自己接管了 WebView2 的拖放，
    // HTML5 的 drop 事件收不到路径，所以由 Rust 侧转发到这里。
    // 回调收到的是绝对路径数组；返回一个取消订阅的函数。
    onFileDrop: function (cb) {
      window.__KALEID_DROP__ = cb;
      return function () {
        if (window.__KALEID_DROP__ === cb) window.__KALEID_DROP__ = null;
      };
    },
    pickFont: function () {
      return internals.invoke('kaleid_pick_font', {});
    },
    listFonts: function () {
      return internals.invoke('kaleid_fonts_list', {});
    },
    readFont: function (path) {
      return internals.invoke('kaleid_font_read', { path: path });
    }
  };
})();
"#;

fn main() {
    // 数据目录与 CLI 完全一致：%LOCALAPPDATA%\Kaleid（可用 KALEID_DATA_DIR 覆盖）
    let data_root = kaleid_core_plugins::data_root();
    let host = match Host::boot(&data_root, false) {
        Ok(host) => host,
        Err(e) => {
            // release 下没有控制台（windows_subsystem），所以同时落一份日志，
            // 否则用户只会看到"程序一闪就没了"。
            let msg = format!("Kaleid 启动失败：{e}");
            eprintln!("{msg}");
            let _ = std::fs::create_dir_all(&data_root);
            let _ = std::fs::write(data_root.join("startup-error.log"), &msg);
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .manage(AppState {
            host: Arc::new(host),
        })
        .setup(|app| {
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Kaleid Launcher Harness")
                .inner_size(1180.0, 760.0)
                .min_inner_size(960.0, 640.0)
                .center()
                .theme(Some(tauri::Theme::Dark))
                .background_color(tauri::window::Color(0x15, 0x15, 0x17, 0xff))
                .initialization_script(BRIDGE_JS)
                .build()?;

            // 把系统拖拽的路径转给界面（Windows 上 Tauri 顶掉了 WebView2 的
            // HTML5 拖放，页面里收不到 dataTransfer 文件列表）。
            let target = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                    let list: Vec<String> = paths
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    let payload = serde_json::to_string(&list).unwrap_or_else(|_| "[]".into());
                    let _ = target.eval(&format!(
                        "window.__KALEID_DROP__ && window.__KALEID_DROP__({payload})"
                    ));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            kaleid_invoke,
            kaleid_bootstrap,
            kaleid_open_dir,
            kaleid_open_url,
            kaleid_set_theme,
            kaleid_after_launch,
            kaleid_pick_folder,
            kaleid_pick_files,
            kaleid_pick_font,
            kaleid_fonts_list,
            kaleid_font_read
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 运行失败");
}
