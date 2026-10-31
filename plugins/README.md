# 声明式插件清单

Kaleid 的第三方插件**不是**任意代码，而是一份签名的 JSON 清单：一串固定动作。
好处是能审计、能离线校验、能保证"停用就能完全撤回"。

> 目录里的清单是**示例**，不参与构建。真正安装时放在
> `<数据目录>/plugins/<id>/manifest.json`，并按下面第 4 节校验签名。

## 1. 最小结构

```json
{
  "id": "example-mirror",
  "name": "自定义镜像源",
  "version": "1.0.0",
  "api": "1",
  "author": "someone",
  "steps": [ ... ]
}
```

| 字段 | 说明 |
|---|---|
| `id` | 全局唯一，只允许 `[a-z0-9-]`，长度 ≤ 48 |
| `name` | 展示名 |
| `version` | 语义化版本 |
| `api` | 宿主 API 主版本；不匹配则拒绝装载 |
| `steps` | 顺序执行的动作列表，见第 2 节 |

## 2. 动作（封闭集合）

| `op` | 参数 | 作用 |
|---|---|---|
| `update-config` | `target`, `patch` | 深合并某个插件的配置（不替换整份） |
| `disable-plugin` | `target` | 停用某插件（等效于补丁里的 `disable`） |
| `enable-plugin` | `target` | 启用某插件 |
| `provide-source` | `id`, `label`, `priority`, `manifestUrl`, `baseUrl` | 再挂一层下载源，**不顶掉**官方源 |
| `bind-slot` | `slot`, `title`, `view` | 往界面某个插槽里加一个只读面板 |
| `set-default-setting` | `key`, `value` | 只在用户没有显式设置时生效的默认值 |

**没有** `exec` / `shell` / `eval` / `!!js` 这类动作，也没有条件跳转、循环。
清单不是图灵完备的，这是刻意的：任何"能跑代码的插件"都等于把整机权限交出去。

## 3. 示例

- [`example-mirror/manifest.json`](example-mirror/manifest.json)：加一个校园镜像源，
  并把官方源降为备份。这就是"修改而非替换"——`version-manager` 仍然在，也仍然工作。
- [`example-panel/manifest.json`](example-panel/manifest.json)：往「游戏设置」插一个只读信息面板。

## 4. 校验顺序

1. `id` / `version` / `api` 形状校验（不合法直接拒绝，没有"尽力而为"）。
2. `steps` 里每个 `op` 必须在封闭集合内，且参数通过该 `op` 的 schema。
3. ed25519 签名：`signature` 覆盖 `steps` 与元数据的规范化 JSON。
4. 逐条 apply，每条注册都产生 `Effect`；插件停用 = 逆序 drop 这些 `Effect`。

任何一步失败，**整份清单回滚**，不留下半装的插件。

## 5. 我们能保证什么

- 清单不会读写任意文件路径：动作只接受"插件 id / 设置 key / 插槽名"。
- 下载源只能是 HTTPS，且不会替换掉内置源（只能用更高 `priority` 排前面）。
- 停用即完全撤回：不会留下"装了一半"的状态。
