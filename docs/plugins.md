# Kaleid 插件文档

> 这份文档只写**代码里真的存在**的接口。Kaleid 没有给插件用的网络端口，也没有
> `exec` / `eval` / `!!js` 这类动作——第三方插件是一份签名的声明式 JSON 清单。

---

## 1. 先弄清三件事

| 问题 | 答案 |
|---|---|
| 插件能监听端口吗？ | **不能**。应用不打开任何给插件用的 TCP/HTTP 监听端口。唯一的调试端口是 [testing.md](testing.md) 里手工启用的 WebView2 `--remote-debugging-port=9336`，它属于测试手段，不是插件 API |
| 插件能调后端吗？ | 模板层走的是宿主命令表（`kaleid_invoke`），但**第三方清单拿不到命令表的调用权**。清单只能通过下面第 4 节的六个动作影响启动器 |
| 插件能改前端吗？ | 只能往**追加型插槽**里加一块只读文本面板（`bind-slot` + `readonly-text:`）。不能注入组件、脚本，也不能替换内置界面 |

内置插件（`version-manager`、`mod-download`、`news` 等）是编译进程序的 Rust 插件，
它们可以注册服务、事件和界面插槽；第三方清单不是同一层东西，权限刻意小得多。

---

## 2. 桌面外壳暴露的原生命令

界面与 Rust 之间只有一层转发，`withGlobalTauri: false`，页面里拿不到任何原生句柄。

| 命令 | 参数 | 说明 |
|---|---|---|
| `kaleid_invoke` | `{ cmd, args }` | 唯一的业务入口，转发到宿主命令表 |
| `kaleid_pick_folder` | — | 原生文件夹选择器，返回路径或 `null` |
| `kaleid_pick_files` | `{ extensions }` | 原生多选文件，返回路径数组 |
| `kaleid_pick_font` | — | 选字体文件（ttf / otf / ttc） |
| `kaleid_font_read` | `{ path }` | 读字体字节（base64） |
| `kaleid_fonts_list` | — | 列系统字体 |
| `kaleid_open_dir` | `{ path }` | 用系统文件管理器打开目录 |
| `kaleid_open_url` | `{ url }` | 用系统浏览器打开链接 |
| `kaleid_set_theme` | `{ theme }` | 同步窗口标题栏主题 |
| `kaleid_after_launch` | `{ action }` | 启动后的窗口动作 |

这些是**外壳**能力，不是插件 API。第三方清单无论怎么配置都调不到它们。

---

## 3. 宿主命令表（`kaleid_invoke` 的 `cmd`）

命令表在 `crates/kaleid-host/src/lib.rs` 的 `dispatch`，界面侧的类型在
`ui/src/bridge/types.ts`。参数一律是 JSON 对象，返回值是 JSON。

### 3.1 文件夹

| 命令 | 主要参数 | 返回 |
|---|---|---|
| `roots_list` | — | `Root[]` |
| `roots_add` | `path`, `alias?`, `force?` | `Root` |
| `roots_remove` | `id` | `null` |
| `roots_rename` | `id`, `alias` | `Root` |
| `roots_select` | `id` | `Root` |
| `roots_set_default` / `roots_clear_default` / `roots_default` | `id` / — / — | `Root` / `null` |
| `roots_set_isolation` | `id`, `mode`（`inherit`/`on`/`off`） | `Root` |

### 3.2 版本

| 命令 | 主要参数 | 返回 |
|---|---|---|
| `versions_scan` | `rootId` | `VersionSummary[]` |
| `versions_remote` | `force?` | `RemoteVersions` |
| `versions_install` | `rootId`, `gameVersion`, `versionName`, `loader`, `installFabricApi?` | `{ jobId }` |
| `versions_delete` | `rootId`, `versionId` | `null` |
| `versions_verify` | `rootId`, `versionId` | 完整性报告 |
| `versions_check_name` | `rootId`, `name` | `{ ok, error }` |
| `versions_loader_status` | `rootId`, `versionId` | 加载器状态 |
| `versions_update_loader` | `rootId`, `versionId`, `loader`, `version` | `{ jobId }` |
| `loaders_versions` | `gameVersion`, `loader` | `LoaderVersions` |
| `loader_version_times` | `gameVersion`, `loader`, `versions` | 发布时间表 |
| `fabric_api_latest` | `gameVersion` | `FabricApiInfo \| null` |

### 3.3 下载

| 命令 | 主要参数 | 返回 |
|---|---|---|
| `downloads_snapshot` | — | `DownloadSnapshot` |
| `downloads_cancel` | `id` | `null` |
| `downloads_clear_finished` | — | `null` |

`DownloadSnapshot.active_workers` 是**真实在途连接数**（单流 1 条、分段每段 1 条），
`concurrency` 是用户设置的并发上限。

### 3.4 设置

| 命令 | 主要参数 | 返回 |
|---|---|---|
| `settings_global` | — | `GlobalSettings` |
| `settings_patch_global` | `patch` | `GlobalSettings` |
| `settings_version` | `rootId`, `versionId` | 版本级设置 |
| `settings_patch_version` | `rootId`, `versionId`, `patch` | 版本级设置 |
| `settings_effective` | `rootId`, `versionId` | 合并后的生效值 |
| `jvm_default_args` | — | `string[]` |
| `app_platform` | — | 平台信息 |

### 3.5 账号与联机

`accounts_list`、`accounts_active`、`accounts_set_active`、`accounts_apply_offline`、
`accounts_remove`、`msa_config`、`msa_start`、`msa_poll`、`msa_cancel`、`msa_states`、
`skin_resolve`。

令牌不出后端：`msa_states` 只说"有没有刷新令牌、几点过期"。

### 3.6 启动与诊断

`launch_plan`、`launch_command`、`launch_start`、`launch_state`、`launch_states`、
`launch_stop`、`launch_cancel`、`launch_check`、`launch_console_tail`、
`crash_report`、`crash_export`、`crash_export_zip`。

### 3.7 内容与模组

`content_dirs`、`content_mods_list`、`content_mods_set_enabled`、`content_mod_icon`、
`content_packs_list`、`content_saves_list`、`content_logs_list`、`content_logs_read`、
`content_import`、`content_delete`、`saves_list`、`saves_delete`、
`saves_datapacks_list`、`saves_datapack_import`、`saves_datapack_delete`、
`mods_types`、`mods_ranking`、`mods_search`、`mods_detail`、`mods_install`、
`mods_target`、`mods_updates_check`、`mods_apply_update`。

### 3.8 Java

`java_list`、`java_inspect`、`java_pick`。

### 3.9 插件与下载源

| 命令 | 说明 |
|---|---|
| `plugins_list` | 内置插件与状态 |
| `plugins_set_enabled` | 启用/停用内置插件（必需插件会拒绝） |
| `plugins_manifests` | 已安装的第三方清单及状态 |
| `plugins_slots` | 清单贡献的只读面板（`bind-slot`） |
| `sources_list` | 合并后的下载源（内置 + 清单贡献） |

---

## 4. 清单格式

安装位置固定：`<数据目录>/plugins/<id>/manifest.json`。

数据目录默认 `%LOCALAPPDATA%\Kaleid`，可用 `KALEID_DATA_DIR` 或 `kaleid --data-dir` 覆盖。

### 4.1 最小结构

```json
{
  "id": "example-mirror",
  "name": "校园镜像源",
  "version": "1.0.0",
  "api": "1",
  "author": "someone",
  "steps": [ ... ]
}
```

| 字段 | 规则 |
|---|---|
| `id` | 只允许 `a-z` / `0-9` / `-`，长度 ≤ 48，**不能与内置插件重名** |
| `name` | 展示名，非空 |
| `version` | 语义化版本，非空 |
| `api` | 必须是 `"1"`，否则拒绝装载 |
| `signature` | 可选。当前构建没有 ed25519 后端，带这个字段的清单会被拒绝（见 6.2） |
| `steps` | 非空，动作列表 |

### 4.2 六个动作（封闭集合）

| `op` | 参数 | 作用 |
|---|---|---|
| `update-config` | `target`, `patch` | 深合并某插件的配置，不替换整份 |
| `disable-plugin` | `target` | 停用某插件；必需插件会被拒绝 |
| `enable-plugin` | `target` | 启用某插件 |
| `provide-source` | `id`, `label`, `priority`, `manifestUrl`, `baseUrl` | 再挂一层下载源，**不顶掉**内置源；两个 URL 必须是 `https://` |
| `bind-slot` | `slot`, `title`, `view` | 往追加型插槽加只读面板；`view` 必须以 `readonly-text:` 开头 |
| `set-default-setting` | `key`, `value` | 只在用户没有显式设置过这个键时生效 |

**没有** `exec` / `shell` / `eval` / `!!js`，也没有条件跳转和循环。

目前唯一的追加型插槽是 `overview.notices`（概览页底部）。`sidebar.versions` 这类内置
插槽是"整块替换"语义，**不开放**给清单使用。

### 4.3 生效时机

| 动作 | 何时生效 |
|---|---|
| `update-config` / `disable-plugin` / `enable-plugin` | 内核装载**之前**改规格表 |
| `provide-source` | 内核装载**之后**注册服务 |
| `set-default-setting` | 内核装载**之后**，且只在 `settings.json` 里没有该键时补 |
| `bind-slot` | 界面调用 `plugins_slots` 时读出来 |

安装/卸载都**下次启动生效**，不做热重载。

### 4.4 校验顺序

1. JSON 结构与字段类型（`deny_unknown_fields`）。
2. `id` / `name` / `version` / `api` 形状。
3. 每个 `op` 在封闭集合内，参数通过该动作的规则。
4. 安装时检查 `id` 不与内置插件重名。
5. 签名策略（见 6.2）。
6. 装载时逐条应用；任一条失败**整份跳过**并记录原因，其它清单继续。

---

## 5. 分发与安装

### 5.1 分发物

就是一个目录：`manifest.json`（可选 `signature`）。没有市场、没有自动更新、
不从 URL 拉取。作者把目录压成 zip 发给用户即可。

### 5.2 CLI 安装

```bash
# 列出内置插件状态与已安装清单
kaleid plugins

# 安装（默认要求签名；本地示例用 --allow-unsigned）
kaleid plugins install ./example-mirror --allow-unsigned
kaleid plugins install ./example-mirror/manifest.json --allow-unsigned

# 卸载（只删 <数据目录>/plugins/<id>，不影响内置插件）
kaleid plugins remove example-mirror
```

换数据目录：

```bash
kaleid --data-dir D:\kaleid-data plugins install ./example-mirror --allow-unsigned
```

安装成功会打印落点，并提示"下次启动生效"。验证：

```bash
kaleid --data-dir D:\kaleid-data sources
```

`example-mirror` 会让 `sources` 多出 `campus-mirror`，同时 `mojang` / `bmclapi` 仍在。

### 5.3 手工安装

把目录复制到 `<数据目录>/plugins/<id>/`，重启启动器。效果与 CLI 相同，
但不会经过 `--allow-unsigned` 这道确认。

---

## 6. 示例

仓库 `plugins/` 下有三份可直接安装的清单。

### 6.1 改前端：加一块只读说明

文件：[plugins/example-panel/manifest.json](../plugins/example-panel/manifest.json)

```json
{
  "id": "example-panel",
  "name": "版本信息面板",
  "version": "1.0.0",
  "api": "1",
  "steps": [
    {
      "op": "bind-slot",
      "slot": "overview.notices",
      "title": "关于这个启动器",
      "view": "readonly-text:这块面板由第三方清单提供。它只读、只能追加。"
    }
  ]
}
```

```bash
kaleid plugins install plugins/example-panel --allow-unsigned
```

重启后概览页底部出现一块卡片。它**不能**：注入 React 组件、执行脚本、替换登录卡
或侧栏、读取账号令牌。

### 6.2 加功能：挂一个校园镜像源

文件：[plugins/example-mirror/manifest.json](../plugins/example-mirror/manifest.json)

```json
{
  "id": "example-mirror",
  "name": "校园镜像源",
  "version": "1.0.0",
  "api": "1",
  "steps": [
    {
      "op": "provide-source",
      "id": "campus-mirror",
      "label": "校园网镜像",
      "priority": 20,
      "manifestUrl": "https://mirror.example.edu/minecraft/version_manifest_v2.json",
      "baseUrl": "https://mirror.example.edu/minecraft"
    },
    {
      "op": "bind-slot",
      "slot": "overview.notices",
      "title": "镜像说明",
      "view": "readonly-text:校园网内优先走本地镜像。"
    }
  ]
}
```

这是**修改而非替换**：`version-manager` 仍在，官方源也仍在，只是多了一条
`priority=20` 的候选。装好后 `kaleid sources` 能看到它。

签名说明：`signature` 字段存在时会被拒绝，因为当前构建没有 ed25519 校验后端。
这不是"跳过校验"，而是**明确拒绝**，避免给用户一种"验过了"的错觉。

### 6.3 加设置：改默认启动行为

文件：[plugins/example-settings/manifest.json](../plugins/example-settings/manifest.json)

```json
{
  "id": "example-settings",
  "name": "默认启动行为",
  "version": "1.0.0",
  "api": "1",
  "steps": [
    { "op": "set-default-setting", "key": "checkBeforeLaunch", "value": true },
    {
      "op": "update-config",
      "target": "version-manager",
      "patch": { "enableMojang": true, "enableBmclapi": true }
    }
  ]
}
```

`set-default-setting` 只在 `settings.json` 里**没有**这个键时生效；用户自己改过的值
不会被覆盖。`key` 必须来自 `GlobalSettings`（见
`crates/kaleid-api/src/model.rs`），`update-config` 的 `patch` 必须是该插件 schema
里已有的键。

---

## 7. 能保证什么，不能保证什么

**能保证**

- 清单不能读写任意文件路径：动作只接受"插件 id / 设置键 / 插槽名 / 下载源 URL"。
- 不能执行代码：封闭动作集，没有 `exec` / `eval` / `!!js`。
- 不能顶掉内置下载源：`provide-source` 只能追加，且必须 `https://`。
- 不能停用必需插件：`disable-plugin` 打到 `java-runtime` 这类插件会整份失败。
- 卸载即完全撤回：删掉目录后，它贡献的源、默认值、面板一起消失。

**不能保证**

- 不能保证清单作者善意：一个恶意清单仍然能把下载源指向攻击者的 HTTPS 服务器。
  这就是默认要求签名、以及要求用户显式 `--allow-unsigned` 的原因。
- 不能替代签名校验：当前构建会拒绝带 `signature` 的清单，请不要把它当成已实现。

---

## 8. 排查

| 现象 | 原因 | 处理 |
|---|---|---|
| `plugins install` 报"没有 signature" | 默认拒绝无签名清单 | 确认来源后加 `--allow-unsigned` |
| 报"带 signature 但无法验证" | 当前构建没有 ed25519 后端 | 让作者给未签名版本，或本地确认来源 |
| 报"与内置插件重名" | `id` 撞了 `version-manager` 之类 | 换一个 `id` |
| 装了但没变化 | 安装/卸载都是下次启动生效 | 重启启动器 |
| `plugins` 里状态是 `invalid` | 清单 JSON 或 `api` 不合法 | 看括号里的原因，改好重装 |
| `sources` 里没有新源 | `provide-source` 的 URL 不是 `https://` | 换成 HTTPS |
| 面板没出现 | 插槽名不是 `overview.notices`，或 `view` 没带 `readonly-text:` | 按第 4.2 节改 |
