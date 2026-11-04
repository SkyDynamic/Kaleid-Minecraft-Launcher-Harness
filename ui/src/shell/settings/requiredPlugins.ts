/**
 * 必需插件：停用它们不是"少一个功能"，而是**启动器直接残废**。
 *
 * 判断依据是依赖关系，不是喜好：`launch` 注入 `settings` 与 `core-lists`，
 * 命令表里几乎每条业务命令都要 `roots` / `versions` / `settings` / `accounts` 服务。
 * `msa-login` 也在这一列：正版登录是启动器的核心路径，关掉它界面上还留着
 * 「添加正版账号」，点了却永远登不上——这种"看得见但用不了"的半残状态比直接不给关更糟。
 *
 * 能安全停用的是 `news` / `version-manager` / `mod-download`：它们各自只
 * 贡献一块能力，停掉后侧栏、账号、启动、Java 挑选、本地内容都还在，只是对应
 * 页面报一句"服务不可用（某插件可能已停用）"。
 *
 * **权威判断在后端**：`PluginSpec::required` 标了的插件，`Kernel::disable`
 * 会直接拒绝，`plugins_list` 返回的 `required` 字段就是同一份事实。这张表
 * 只补一件事——"为什么不能关"的那句话，因为后端描述里放不下这么长的中文。
 * 所以表里多一个后端没标的 id 不会让开关变成可关：界面取两者的并集（`required`
 * 字段或这张表命中），宁可多拦一个，也不要出现"点了没反应"。
 */
export const REQUIRED_PLUGINS: Record<string, string> = {
  'launcher-logger': '记录启动器、游戏启动、下载与插件调用的诊断日志，是始终启用的内置插件',
  'core-lists': '侧栏文件夹列表由它提供，停用后无法收录或选择文件夹',
  'version-store': '版本扫描由它提供，停用后版本列表为空，也就没有可启动的目标',
  settings: '分层设置解析由它提供，停用后无法生成启动参数',
  accounts: '全局账号模型由它提供，停用后无法登录，也生成不了启动计划',
  launch: '启动计划由它生成，停用后「启动游戏」不可用',
  'java-runtime': 'Java 运行时发现由它提供，停用后挑不出该用哪个 Java，游戏启动不了',
  'game-content': '本地内容读取由它提供，停用后模组/资源包/存档/日志四个标签全空',
  'msa-login': '正版账号登录由它提供，停用后无法登录微软账号，只能离线启动',
};

export function isRequiredPlugin(id: string): boolean {
  return Object.prototype.hasOwnProperty.call(REQUIRED_PLUGINS, id);
}

export function requiredReason(id: string): string | undefined {
  return REQUIRED_PLUGINS[id];
}
