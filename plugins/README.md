# 声明式插件清单

完整文档在 [docs/plugins.md](../docs/plugins.md)：接口、六个动作、三个可安装示例，
以及 `kaleid plugins install / remove` 的流程。

这里只放**能直接安装的示例**，不参与构建：

| 目录 | 演示什么 |
|---|---|
| [`example-mirror`](example-mirror/manifest.json) | 挂一层校园镜像源（`provide-source`），并加一块只读说明 |
| [`example-panel`](example-panel/manifest.json) | 往概览页底部追加只读面板（`bind-slot`） |
| [`example-settings`](example-settings/manifest.json) | 默认启动行为（`set-default-setting` + `update-config`） |

安装：

```bash
kaleid plugins install plugins/example-mirror --allow-unsigned
```

三条硬边界：

1. **没有给插件的网络端口**，也没有 `exec` / `eval` / `!!js`。
2. 只能**追加**，不能替换内置界面或内置下载源；必需插件不能停用。
3. 安装/卸载都**下次启动生效**；当前构建会拒绝带 `signature` 的清单（没有 ed25519 后端）。
