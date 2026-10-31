<p align="center">
  <strong>中文</strong> · <a href="./README.en.md">English</a>
</p>

<p align="center">
  <img src="./docs/brand/kaleid-mark.png" alt="Kaleid MC Launcher Harness" width="420" />
</p>

# Kaleid MC Launcher Harness

一个 Minecraft 启动器：所有能力都是插件

它不替换你已经装好的游戏目录。后加入的插件再挂一层——例如换一个下载源——而不是必须顶掉前一个。配置不合法、版本 JSON 损坏、插件装载失败，都会明确报错，不会静默重置你的数据。


## 怎么跑

在仓库根目录：

```powershell
cargo test --workspace
cargo run -p kaleid-cli -- --help
dev.bat
```

`dev.bat` 进入 `src-tauri` 执行 `tauri dev`，这是带系统 WebView2 的外壳。只看界面、不连宿主时：

```powershell
pnpm --dir ui dev
```

数据目录默认是 `%LOCALAPPDATA%\Kaleid`。可以用 `KALEID_DATA_DIR` 或 `kaleid --data-dir` 换掉。账号和版本级设置写在这里，不改游戏目录里已有的文件。

测试怎么读、哪些用例要真机，见 [docs/testing.md](docs/testing.md)。

## 仓库地图

| 路径 | 职责 |
|---|---|
| `crates/kaleid-kernel` | 服务链、可回收副作用、事件、补丁组合 |
| `crates/kaleid-api` | 服务契约与 DTO。内核和插件之间的公共语言 |
| `crates/kaleid-core-plugins` | 内置插件 |
| `crates/kaleid-host` | 命令表。界面只打到这里 |
| `crates/kaleid-cli` | 无头验收：收录、扫描、打印启动参数 |
| `src-tauri` | 外壳。不在 workspace 里，核心测试不必拖上 Tauri |
| `ui` | React 19、Vite 7、Tailwind v4。不用组件库 |
| `presets` | 组合补丁，只写差异 |
| `plugins` | 声明式清单的例子，不参与构建 |
| `docs/brand` | 标。README 用透明底；白底是 `kaleid-mark-white.png` |

设计规格在 [docs/superpowers/specs/2026-10-29-kaleid-launcher-design.md](docs/superpowers/specs/2026-10-29-kaleid-launcher-design.md)。清单的形状见 [plugins/README.md](plugins/README.md)。

## 许可

[Apache License 2.0](LICENSE)。版权与第三方组件见 [NOTICE](NOTICE)。

Minecraft 的名称与内容归 Mojang Studios / Microsoft 所有。本项目不分发游戏本体。
