# 怎么跑测试

全部命令在仓库根目录执行，工具链都已在 PATH 上（cargo 1.97 / node 24 / pnpm 11）。

## 1. 全部 Rust 测试

```powershell
cargo test --workspace
```

期望输出：

| crate | 测试数 |
|---|---|
| `kaleid-kernel` | 49 |
| `kaleid-api` | 21 |
| `kaleid-core-plugins` | 456（410 单元 + 46 集成；其中 15 个手动真机 / 联网用例标了 `#[ignore]`，默认跳过） |
| `kaleid-host` | 38 |
| `kaleid-cli`（`tests/flow.rs` 集成） | 9 |

当前默认工作区套件为 **558 通过、15 跳过**；浏览器 mock 另有 2 条 Node 回归测试。

> `kaleid-core-plugins` 的 410 个单元里，几块值得单独记一笔：4 个 NBT、7 个 Java 运行时、
> 21 个本地内容、24 个微软登录（响应解析、XErr 翻译、DPAPI 令牌存储、续期时"先落盘
> 再打通 XBL"的顺序）、6 个下载重试/分段/阶段，以及两个「校验只看 `.minecraft` 根目录」
> 的回归用例。
>
> 「第三方启动器产出的版本为什么起不来」那一类问题（详见 6.6）现在有 3 条回归用例：
> `coordinate_only_libraries_are_kept_and_deduplicated`（只有 Maven 坐标的库要进
> classpath，同一个库的两个版本只能留一个）、
> `empty_argument_values_survive_and_clientid_is_substituted`（`--xuid ""` 这类空值
> 参数不能掉，`${clientid}` 必须替换）、`maven_name_falls_back_to_a_libraries_path`
> （坐标 → 路径的四种写法）。
>
> 这一轮新增的三块自带测试：
> - `jvm.rs`（10 条）：默认四条与设置默认值一致、ZGC/G1 档位的 Java 版本门槛与降级说明、
>   实验性选项必须先 `-XX:+UnlockExperimentalVMOptions` 解锁、用户自己写了
>   `-XX:+Use*GC` 时内置档位整体让位。
> - `options.rs`（8 条）：`lang:` 原位替换/追加、其它键一字不动、YOSBR 优先、
>   有存档也照做（开关就是授权）、已中文 no-op、CRLF 文件保持 CRLF、1.10 及更早写 `zh_CN`。
> - `gpu.rs`（4 条）：路径不存在或为空只报错不 panic、「写不进去只是警告」这条纪律、
>   能读到真实 Windows build 号。
>
> 「默认 JVM 参数」那件事横跨三层，测试也分三层钉住：
> `launch.rs` 的 `default_args_come_from_settings_not_from_a_hidden_injection`
> （设置里有就有、清空就真的没有）、`settings.rs` 的
> `legacy_empty_jvm_args_are_seeded_once` / `legacy_custom_jvm_args_are_left_alone`
> （老配置补一次默认值，之后空就是空）、宿主的
> `jvm_defaults_are_exposed_and_prefilled`（`jvm_default_args` 命令与
> `settings_global` 的初值一致）。
>
> 多实例那组（`game.rs`）在 Windows 上用 `cmd /c ping` 当"游戏"：两个版本能同时跑、
> 会话 id 递增、日志目录互不覆盖、`LatestLaunch.bat` 会落盘、排障包也带上它、
> 且抹掉访问令牌。
>
> PCL2 式启动流程（2026-10-31）新增的 14 条：`repair.rs` 5 条（齐全不算缺、
> 坐标库要拼 maven 地址、别的平台的 natives 不算缺、资源对象按 hash 对、
> 索引缺失优先下载）、`crash_zip.rs` 4 条（文件名没有冒号、CRC-32 对得上
> `0xCBF43926`、写完能独立解回来、落盘大小合理）、`game.rs` 5 条（五步按序推进
> 且缺少文件那步是"跳过"、准备阶段取消落到 `Cancelled`、进程起来后取消要被拒绝、
> 进过游戏再崩不退化成"启动失败"、"本次日志"按字符边界截断）。另外 `crates/kaleid-host` 的
> `launch_progress_commands_are_wired` 钉住三个新命令的 camelCase 参数与"没有会话时"
> 的人话错误。
>
> Forge / NeoForge 安装器不再自己去 `maven.neoforged.net` / `maven.minecraftforge.net`
> 下库（国内超时后只甩一串坐标，界面上是「非法输入：安装器失败」）。现在先读安装器
> 里的 `install_profile.json` + `version.json`，经 BMCLAPI `/maven/` 补进
> `staging/libraries`，安装器看到校验通过的文件就跳过联网。Fabric 的加载器库同样带
> BMCLAPI 镜像。真机回归：`KALEID_NEOFORGE_STAGING=<失败留下的 staging> cargo test -p kaleid-core-plugins --lib install::tests::real_neoforge_installer_succeeds_after_bmclapi_prefetch -- --ignored`，
> 期望日志末尾是 `Successfully installed client into launcher.`。
>
> 「退出 ≠ 崩溃」那 8 条（见 6.8）：`crash.rs` 5 条（正常开局日志里的 OpenGL 行不能
> 算崩溃、关窗退出不算、只有关键词命中不算、非零退出且没关服日志才算、`Stopping!`
> 之后拿到非零码也不算），`game.rs` 3 条（「结束进程」不算崩溃、真崩了窗口那步仍保持
> 完成、上一次留下的 crash-reports 不挂到这一次上）。
>
> 2026-11-01 这一轮（老版本启动 / NeoForge 安装 / OptiFine / 离线中文名）新增 18 条，
> 另有 2 条联网用例标了 `#[ignore]`：
> - `natives.rs` 5 条：classifier 按当前系统挑选、`rules` 过滤掉别的平台、
>   坐标 → `libraries` 路径、同一库的多个 classifier 不互相覆盖、
>   64 位下 32 位 natives 不算。
> - `launch.rs` 4 条：legacy 画像能拼出完整命令行、`${auth_session}` /
>   `${user_properties}` 不残留占位符、natives 目录落在 `versions/<id>/natives`、
>   classifier 的 natives jar 只解压不进 classpath。
> - `install.rs` 4 条 + 1 条画像用例：安装器必须是 zip（不是 HTML 错误页）、
>   安装器失败提示指向下载源、OptiFine 的现代 / legacy 分界（1.13 起是 mod）、
>   Fabric 与 NeoForge 不能配 OptiFine、配 Forge 时只往 `mods/` 放一条、
>   单装时画像换成 `launchwrapper` + `OptiFineTweaker` 并留 `.optifine` 标记。
> - `downloads.rs` 1 条：`prefer_mirror` 把 BMCLAPI 排前面，且不破坏「仅官方」。
> - `loaders.rs` 1 条：BMCLAPI 的 OptiFine 列表被翻成由新到旧，`pre` 版不算稳定版。
> - `versions.rs` 在既有的分类用例里加了三条断言：单装 OptiFine 归到 OptiFine 组、
>   版本号去掉游戏版本前缀、Forge + OptiFine 仍算 Forge。
> - `accounts.rs` 1 条：中文名可设、uuid 按 Java 规则算、改回英文名仍复用同一档案。
> - `kaleid-api` 的 `offline_name_rules` 与 `kaleid-cli` 的
>   `offline_name_rules_match_api_contract` 一起钉住 16/17 位边界、混合中英、
>   假名与空格：**汉字放开、其它非 ASCII 仍拒绝**。
>
> `tests/download_retry.rs` 有 16 个集成用例：进程内起一个最小 HTTP/1.1 服务，验证
> 「连接被掐断后重试」「连续 5xx 后重试」「服务器忽略 `Range` 时退回单流」
> 「支持 `Range` 时并发多连接且拼接后 SHA-1 一致」「SHA-1 不符不重试」
> 「连不上时给出人话错误」，另外几条钉住调度与限流：快照里的连接数必须等于
> 真实连接数（不能一边跑着 8 条连接、一边显示 1/64）、并发 32 只剩一个文件时
> 要开出 8 条连接（旧实现写死 4 条）、429 带 `Retry-After` 时等够再重试同一个地址、
> 一直被限流才换源、最大的文件至少占一半连接预算、换源后重新探测 `Range`；
> 另外 4 条钉住「安装器」那类**未知大小**的文件（清单里 `size: 0`）：学到大小后转分段、
> 服务器不认 `Range` 就退回单流、走跳转镜像也能下完、以及**分段跑到一半某条连接
> 落到不认 `Range` 的节点上（能力中途翻转）不能把任务卡死**。
> **不联网**，所以留在默认套件里。
>
> `tests/remote_metadata.rs` 有 7 个（6 个离线、1 个联网默认跳过）：镜像返回「200 + HTML」
> 或语法损坏的 JSON 时换源，全部损坏时列出各地址的错误；内容错误每个地址只试一次，
> 超过上限的响应直接拒收，正常 JSON 照常解析。联网用例单独验证 26.3 的索引路径。
>
> 26.3 资源索引 / 安装事务这一轮新增 5 条：
> - `mirrors.rs`：`piston-meta.../v1/packages/<sha>/34.json` 映射到 BMCLAPI 的
>   `/v1/packages/...`，不再走 `/version/<id>/assetIndex`；`files.minecraftforge.net/maven`
>   与 `maven.neoforged.net/releases` 也按 PCL2 规则映射；
> - `install.rs`：安装失败/取消后版本目录消失、安装标记目录可同名接管、有存档时不接管、
>   `.sha1` 只接受 40 位十六进制、26.3 索引走 packages 路径；
> - `versions.rs`：带 `.kaleid-incomplete` 的目录不进入版本列表。
>
> 这一轮新钉住的两处后端行为：
> - `install.rs` 4 条：安装失败后 `versions/<名字>/` 整个消失、已存在的版本目录一个
>   字节都不动、成功时文件都在、`sweep_staging` 只清超过阈值的空壳；
> - `downloads.rs` 5 条 `plan_urls`：镜像只是转回官方时直接用官方地址、终点是别的
>   节点时用节点、小文件不解析、显式策略顺序不变、镜像冷却时官方优先；
> - `msa.rs` 4 条：访问令牌过期不等于掉登录（能续就算有效）、`invalid_grant` 与网络
>   故障分开、续期时新刷新令牌**先落盘**再打通 XBL、微软拒绝时只标记不删记录。

> 校园镜像兜底（2026-11-02）这一轮新增 19 条，另有 1 条联网用例标了 `#[ignore]`：
> - `mirrors.rs` 8 条（1 条联网）：内置 BMCLAPI 地址都挂在主站常量下、八个校园镜像
>   互不重复且都是 https + `/bmclapi`、`/maven/` 要剥掉前缀而 `/assets/` 保留命名空间、
>   只有静态文件树（`/maven/`、`/assets/`）才展开出校园地址、第三方镜像一个都不碰、
>   主机与路径小工具（带端口的 `:443` 也算主站）、官方地址能映射到 BMCLAPI 等价地址；
>   联网那条 `real_campus_mirrors_serve_maven_files` 打到 8 家校园镜像的同一个 jar，
>   要求**至少 3 家**回的是真正的 jar 头 `PK\x03\x04`（校园网外 ustc 会 403，所以不要求全绿）。
> - `downloads.rs` 8 条：校园镜像跟在主站后面且官方排在整组外面、「仅官方」下校园镜像
>   一个都不出现、「仅镜像」保住整组、关掉设置项只剩主站一个地址、换源时跳过刚被记过
>   的校园主机但**队尾地址永远保留**、派活前跳过"主站冷却中 / 镜像被记过"的地址、
>   整组镜像的内部顺序一个字都不改、只有校园主机进"十分钟别选它"名单。
> - `hostlimit.rs` 1 条：被记过的校园主机不会被选源器挑中，10 分钟后自己恢复。
> - `repair.rs` 1 条：启动前补全的资源对象与依赖库都带 BMCLAPI 地址（旧实现里
>   `mirror_url_for` 只认 Modrinth / CurseForge，游戏文件一条镜像都匹配不上）。
> - `settings.rs` 1 条：`downloadCampusMirrors` 缺省开、关掉能落盘、重开能读回来。
>
> 只有校园镜像会被写进"十分钟别选它"的名单（`only_campus_hosts_get_blacklisted`）：
> 主站是唯一有全套文件的来源，把它拉黑等于放弃安装。

> `src-tauri` 故意不在 workspace 里（它要拉整套 Tauri 依赖），
> 所以上面这条命令始终是秒级，可以在改代码时反复跑。

### 启动器日志回归

```powershell
cargo test -p kaleid-kernel
cargo test -p kaleid-core-plugins logger
cargo test -p kaleid-host
cargo test -p kaleid-cli --test flow
node --test ui/tests/required-plugins.test.mjs
```

浏览器 mock 的必需插件开关使用 Node 24 内置测试运行器直接验证（不增加前端测试框架）；
确认拒绝停用后没有状态修改或事件广播，同时普通新闻插件仍可正常切换。

启动器日志由必需插件 `launcher-logger` 提供，和游戏自己的 `console.log` / `latest.log`
分开。测试使用临时数据目录、本地 HTTP fixture 与可控的小轮转阈值，不依赖外网，
不需要真实 Minecraft 安装，也不需要把真实磁盘写满。
观察者 / 写入器的 panic 测试在 debug unwind 构建中运行，只验证 `catch_unwind` 能处理
可展开的 panic。根项目和桌面端的 release 配置仍为 `panic = 'abort'`，这类 panic 会
终止进程，测试不承诺从任意 release panic 恢复；可预期的 I/O 失败仍按 fail-open 处理。

验收重点：
- 标准 / minimal 组合都有该插件；界面开关不可点，运行时、清单与补丁停用均拒绝，
  不能用移除或条件过滤绕过必需标记。
- 真实 CLI 退出后即可读取 `<数据目录>/logs/launcher/<运行 ID>/launcher.log`；
  并发桌面 / CLI 的目录独立，同一内核重复初始化不重复订阅或写入。
- JSON Lines 有运行、操作、会话 / 任务 / 批次关联，后台开始之后仍能记录真正的结果；
  下载批次完成不被误写成整个安装完成，取消请求不冒充已经终止。
- 轮转保留当前文件与四份归档；七天清理跳过仍加锁的活跃会话、未知条目与游戏日志。
  此项按启动器管理可信目录、仅协作实例且清理期间无外部并发运行目录 / 祖先重命名或符号链接 /
  目录联接替换的模型验证，不承诺抵御任意同用户文件系统替换。
- 队列满、关闭、文件不可写或写入失败时，原来的启动 / 下载结果不变，退出不无限等待；
  恢复时有丢失计数，正常退出会限时排空。
- 用哨兵令牌、登录码、JVM 参数、URL 凭据 / 查询串、响应片段与控制字符验证不泄密、
  不伪造新日志行；Unicode 截断后仍是有效 JSON，单条不超过 8 KiB。

手工烟雾检查：用 `KALEID_DATA_DIR` 或 `--data-dir` 指向临时目录，启动一次游戏、
安装 / 取消一次版本，再同时运行一次 CLI 命令。检查插件页的「必需」说明、各运行的
日志目录、真正的退出 / 安装结果和原有游戏控制台文件。不要把真实账号令牌贴进测试报告。

### 提交前安全回归

本轮新增 16 条离线回归：14 条安装器测试和 2 条元数据集成测试。使用临时目录与本地 HTTP
服务，不访问真实游戏或账号目录：

```powershell
cargo test -p kaleid-core-plugins --lib correction_
cargo test -p kaleid-core-plugins --test remote_metadata
```

覆盖的边界：
- 只有本启动器标记且布局明确的半成品可以自动回收；不同名称的 JSON、mods/config、未知条目
  和未标记目录都保留。安装预检与启动清理使用同一判断。
- 每个候选先通过 JSON、资源索引大小和 SHA-1 校验再被接受；坏内容直接换源，成功响应只取一次。
- 必需模组发布失败不能报告完成；不同内容的已有文件不覆盖，相同文件复用，后续失败只回滚
  本次新建的共享文件。正常错误可回滚，不承诺多路径操作在崩溃或 abort 时原子完成。

### Forge 跨版本依赖回归

`install_forge_tests.rs` 的默认离线测试覆盖四种真实安装器布局：

| Minecraft / Forge | 验证点 |
|---|---|
| 1.12.2 / 14.23.5.2860 | 两份元数据重复列出的内置 Forge JAR 不进入网络队列 |
| 1.16.5 / 36.2.42 | 内置 main / universal JAR 的空 URL 不触发镜像请求或 `RelativeUrlWithoutBase` |
| 1.20.1 / 47.2.0 | 正常远程 Forge 依赖仍预下载，保留 URL、校验、大小与目的路径 |
| 1.21.4 / 54.1.22 | 安装器生成的 client JAR 不预下载 |

规则看元数据而不看版本号：**显式空字符串 / 纯空白的 artifact URL** 代表交给官方安装器提取或生成。
缺少 URL / `null` 不等同于此标记，仍保留仓库基址和坐标镜像兜底。另有 synthetic JAR
用例走真实 `prefetch_installer_libraries` 边界，确认纯本地依赖不启动下载批次。

```powershell
cargo test -p kaleid-core-plugins --lib install::tests

# 联网 + Java：两次完整安装（含资源），各自使用独立临时数据 / 游戏目录，不启动游戏。
cargo test -p kaleid-core-plugins --lib install::tests::real_forge_ -- --ignored --nocapture --test-threads=1

# 也可分别验证；默认自动发现 Java 8 / 17。需要指定运行时时，设置 KALEID_JAVA。
cargo test -p kaleid-core-plugins --lib install::tests::real_forge_1_16_5_installs_with_bundled_libraries -- --ignored --nocapture
cargo test -p kaleid-core-plugins --lib install::tests::real_forge_1_20_1_installs_with_remote_libraries -- --ignored --nocapture
```

完整安装用例验证成功状态、自包含版本画像、Forge/FML 运行时文件大小 / SHA-1，以及 1.16.5
两份内置 JAR 和 1.20.1 远程 universal JAR 的独立已知校验值。1.20.1 的 universal 仅在
安装器 profile 中列出，不能要求它出现在最终运行时画像里。用例每 30 秒报告下载进度，十分钟
后请求取消（不是硬杀 Java，安装器仍受原有超时约束）。网络或 Java 条件不满足时用例**失败并报原因**，
不悄悄跳过或算通过。这些用例默认 `#[ignore]`，不会给离线工作区测试引入外网请求；
临时目录自动清理，不读取现有账号。

2026-11-04 验证记录：Forge 修复阶段工作区 **542 通过、15 跳过**；提交前安全修正后
为 **558 通过、15 跳过**，四种 Forge 布局的离线回归均通过。下面的联网结果未因此重算为通过。
两条完整联网安装均在资源仍未下完时触发十分钟预算并取消，**不算完整安装通过**。
其中 1.20.1 / 47.2.0 的官方安装器阶段已成功，迁入版本目录的 universal JAR 实测为
2,672,123 字节、SHA-1 `ac078f159add92ef9dce7a8259508c9d78470d3a`，与官方元数据一致。
1.16.5 的完整联网安装未完成验收，不能用离线回归替代该结果。

### 1.1 真机用例（`#[ignore]`，手动跑）

这四条会碰真实系统（注册表 / 真实游戏目录 / 真的执行 .bat / 真的起 JVM），
所以不进默认套件：

```powershell
cargo test -p kaleid-core-plugins --lib gpu::tests::real_registry_roundtrip -- --ignored
cargo test -p kaleid-core-plugins --lib game::tests::rendered_bat_runs_in_cmd -- --ignored
cargo test -p kaleid-core-plugins --lib options::tests::real_options_file_roundtrip -- --ignored --nocapture
cargo test -p kaleid-core-plugins --lib jvm::tests::generated_args_are_accepted_by_real_java -- --ignored --nocapture
```

第一条对 `notepad.exe` 写 / 读 / 删 `GpuPreference` 后清干净（不碰启动器和游戏）；
第二条把渲染出的 `LatestLaunch.bat` 交给真 `cmd` 跑通，验证 `%%` 转义与引号；
第三条拿这台机器上真实的 `options.txt` 往返一遍，确认只改 `lang:`、行数与 CRLF 都不变；
第四条把四个 GC 档位生成出来的参数分别交给本机每个 Java 跑 `-version`——这条正是
抓出「`G1NewSizePercent` 是实验性选项、必须先用 `-XX:+UnlockExperimentalVMOptions` 解锁」
那个坑的用例。

### 1.2 联网冒烟测试（`#[ignore]`，默认不跑）

模组下载依赖 Modrinth / CurseForge（经 MCIM 镜像）的 JSON 字段名，字段一改单测
里的手写 fixture 发现不了，所以另有一条真发请求的用例：

```powershell
cargo test -p kaleid-core-plugins --test mods_live -- --ignored --nocapture
```

它会打印 `ranking / search / groups / files / warnings`，并顺带走一遍 CurseForge
分支（官方接口无 API Key → 镜像 + 前置解析）。**需要外网**，所以不进默认套件。

OptiFine 与 NeoForge 安装器各有一条「源到底通不通」的用例（都是真的发请求）：

```powershell
# BMCLAPI 的 OptiFine 列表 + jar 直链：1.12.2 → HD_U_G5，1.21.4 → HD_U_J3
cargo test -p kaleid-core-plugins --lib real_optifine -- --ignored --nocapture
# NeoForge 安装器在官方 Maven 不可达时能否从 BMCLAPI 拿到完整 jar（约 3.5 MiB）
cargo test -p kaleid-core-plugins --lib real_neoforge_installer_downloads_via_bmclapi -- --ignored --nocapture
```

这两条打印的是真实字节数，用来确认「官网抓不动、镜像有同样的文件」这个前提还成立。

八家校园 BMCLAPI 镜像还同不同步、有没有抽风，用这条探一遍（同一个 jar 打到全部镜像
上，逐个打印头四个字节并检查是不是 `PK\x03\x04`；要求至少 3 家给出真 jar）。
2026-11-02 实测：七家通过，ustc 从校园网外 403，而**主站当天把请求 302 到
`chinanet.mirrors.ustc.edu.cn` 后也被 403**——正好说明这层兜底的必要性。

```powershell
cargo test -p kaleid-core-plugins --lib real_campus_mirrors_serve_maven_files -- --ignored --nocapture
```

## 2. 只跑一部分

```powershell
cargo test -p kaleid-core-plugins              # 单个 crate
cargo test -p kaleid-core-plugins --test download_retry  # 只跑下载重试/分段（不联网）
cargo test -p kaleid-cli --test flow            # 单个集成测试文件
cargo test -p kaleid-kernel effect_runs_on_drop # 按测试名过滤
cargo test --workspace -- --nocapture           # 带 stdout
```

## 3. 界面

```powershell
pnpm --dir ui build        # = tsc --noEmit && vite build，类型检查 + 产物
pnpm --dir ui dev          # http://localhost:5373，浏览器里用 mock 数据
```

界面在浏览器里跑的是 `ui/src/bridge/mock.ts`（内存假数据）。
真机连 Rust 侧的验证见第 5 节。

### 3.1 扩充模组中文名词表

```powershell
node scripts/mod-names/build-names.mjs --limit 60 --dry-run   # 先看会改什么
node scripts/mod-names/build-names.mjs --limit 60             # 写回种子词表
node scripts/mod-names/verify-slugs.mjs                       # 校验 slug 指向是否正确
```

脚本从 MCIM 排行取热门模组，去 MC 百科按英文名搜索，**只有英文名归一化完全
相等**才收录（结果标题形如 `钠 ( Sodium )`；结果没带英文名时会再读一次条目页
标题核对）。串行 + 1.2s 间隔，几百个候要约几分钟，属于开发期一次性工作。
`verify-slugs.mjs` 再回头核对每条 slug 在站上真实存在且标题一致，对不上就丢掉
slug（宁可少一条精确匹配，也不要把别的模组的中文名贴过来）。

> 中文名有三层：随包种子 → 数据目录 `<数据目录>/mod-names-zh.json` 覆盖 →
> 这个脚本离线扩库。运行时不会抓任何页面。

## 4. 界面纯函数自检

`ui/src/lib/*` 里跟渲染无关的部分是纯函数，可以直接在 Node 里跑（Node 22+ 自带 TS 类型擦除）：

```powershell
node scripts/log-format-check.mjs      # 日志着色解析
node scripts/time-format-check.mjs     # 时间格式化（秒 / 毫秒 / ISO 串）
```

退出码非 0 表示有用例失败。日志那份覆盖：级别识别（log4j 布局 / 裸 `[INFO]` / 无级别）、
时间戳-线程-logger 是否被正确切段、堆栈续行、ANSI 分段与加粗、**拼回去等于原文**、
错误/警告计数、超出渲染上限只留尾部。时间那份覆盖：秒与毫秒渲染一致、2100 年不被误判成毫秒、
空值/`NaN`/乱字符串不炸，以及**存档的「最后游玩」不会再出现 5 位年份**。

## 5. 体积预算闸门

```powershell
node scripts/size-budget.mjs            # 只报告
node scripts/size-budget.mjs --strict   # 超闸门值直接 exit 1（给 CI 用）
```

当前实测：主 chunk 144.2 KB gzip（目标 220 / 闸门 300）、安装包 1.23 MiB
（安装包数字来自上一次 `tauri build`，改界面不会动它）。

## 6. 手工验收

### 6.1 CLI 冒烟（不启动界面，验证整条链路）

```powershell
$data = "$env:TEMP\kaleid-try"
cargo run -p kaleid-cli -- --data-dir $data roots add "$env:APPDATA\.minecraft" --alias 主力
cargo run -p kaleid-cli -- --data-dir $data versions
cargo run -p kaleid-cli -- --data-dir $data accounts off Player_123
cargo run -p kaleid-cli -- --data-dir $data plan 1.21.4      # 打印完整启动命令行
cargo run -p kaleid-cli -- --data-dir $data --minimal plugins # version-manager 应为 Disabled
```

`--data-dir` 让 CLI 与桌面版共用同一份数据；`--minimal` 是降级组合。

### 6.2 桌面版真机桥接（确认界面真的连到 Rust，而不是退回 mock）

```powershell
cd src-tauri; cargo build --release; cd ..
$env:KALEID_DATA_DIR = "$env:TEMP\kaleid-try"
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9336"
Start-Process src-tauri\target\release\kaleid-desktop.exe
Start-Sleep 5
node scripts/probe-webview.mjs 9336 "return JSON.stringify({url: location.href, hasBridge: !!window.__KALEID__})"
```

期望：`{"url":"http://tauri.localhost/","hasBridge":true}`
——`tauri.localhost` 表示内嵌的是 `ui/dist`，`hasBridge` 为 true 表示没退回 mock。

### 6.3 打包

```powershell
cd src-tauri; npx --yes @tauri-apps/cli@^2 build
```

产物：`src-tauri/target/release/bundle/nsis/Kaleid_0.1.0_x64-setup.exe`。

### 6.4 界面改动的手工验收（`pnpm --dir ui dev` 里点一遍）

| 检查项 | 期望 |
|---|---|
| 侧栏底部「设置」/ `Ctrl ,` | 打开设置 Dialog，左侧三个分区：APP 设置 / 游戏全局 / 插件 |
| Dialog 关闭 | `Esc` 或点遮罩、点 ✕ 都能关；关闭有 130ms 退出动画 |
| 主区「游戏设置」页签 | 仍然存在，且**没有**被搬进 Dialog（版本级覆盖留在原处） |
| APP 设置 → 界面动效 | 选「关闭」后 `<html data-motion="off">`，所有过渡变成 0.01ms；刷新后仍是关闭 |
| APP 设置 → 主题 | 三个选项：跟随系统 / 深色 / 浅色。选「深色」`<html data-theme="dark">`、选「浅色」变 `light`，整窗换肤且刷新后保持；选「跟随系统」时改 Windows 亮/暗模式，界面立即跟着变 |
| 设置行排版 | 每行只有「标签（+ 一句说明）」和右侧控件，**没有**作用域小标签；控件贴齐行尾右对齐、与标签垂直居中（滑块这类宽控件占满整列后右端同样对齐）；开关类的文字在开关**左边** |
| APP 设置 → 关于 | 上下结构的信息块（不是设置行）：软件版本 / 开源协议 / 版权信息 / 开源许可，版权行为 `© 2026 Sky_Dynamic` |
| 主题切换的连带项 | 滚动条、滑块轨道/滑块本身、开关、分段控件选中格、输入框、代码块、任务进度槽、遮罩都在浅色下可辨认（没有残留的深色块） |
| 插件分区 | 「新闻」「版本管理」「模组下载」能停用；其余 10 个（含「Java 运行时」「本地内容」「启动游戏」「存档管理」）带「必需」chip，`Switch` 为 disabled |
| 停用「版本管理」 | 侧栏目录/版本区退回只读占位（slot 反注册）；安装向导与下载队列不可用；**游戏全局设置里的「下载并发」「下载源」两行一起消失**，重新启用后回来 |
| 停用「新闻」 | 右栏上半新闻直接消失，下载队列自动占满整列（**不留占位**）；重新开启后恢复 |
| 右栏上半 | 内置新闻插件默认开启，Minecraft 新闻 5 条，缩略图来自 `launchercontent.mojang.com` |
| 右栏新闻断网 | 有缓存 → 顶部出现琥珀色「离线 · HH:mm」；无缓存 → 「新闻暂时取不到」+ 重试（**不会**出现假数据） |
| 右栏下半 | 真实任务队列（不是占位）；整块可点，点开「下载队列」Dialog 看总进度与每个子项 |
| 添加文件夹 | 真机上「浏览」或点击路径框会打开 Windows 原生文件夹选择器；选中后回填路径并自动填别名，不使用网页内伪造选择器 |
| 「获取新版本」 | 打开三步向导；第一步四个可折叠卡片列表（正式版/预览版/远古版/愚人节版），预览版默认展开显示，没有「显示快照」开关；版本卡左侧为无圆角、平滑缩放的方块图标，右侧版本号与 `yyyy/MM/dd hh:mm` |
| 安装向导 → 命名 | 第二步顶部左箭头返回；版本名即时校验（末尾小数点、Windows 非法字符、重名）；第三步也能改同一个名字 |
| 安装向导 → 步骤条回退 | 到第三步后点顶部「选择版本」/「加载器」，能回到对应前置步骤；当前与未来步骤是 disabled，不能往前跳 |
| 安装向导 → 加载器 | 进入第二步时 Fabric / Forge / NeoForge 三份版本列表**同时**加载（各自计数为 3）；单个加载器请求失败时另外两个仍可选择；展开分组后显示发布时间，加载中为「时间加载中…」 |
| 安装向导 → NeoForge 26.x | 选择 `26.1` / `26.1.1` / `26.1.2` / `26.2` / `26.3` 时 NeoForge 分组能读出对应 `26.x.y.*` 版本；官方 Maven 不可达时 BMCLAPI `maven-metadata.xml` 兜底，26.1 系列的测试版也可见 |
| 安装向导 → 加载器锁定 | 展开分组只显示版本、不自动选择；点具体版本才选中；选中 Fabric/Forge/NeoForge 后其它组禁用并自动收起；再次点击已选版本回到原版，且当前加载器列表保持展开；已有同版本加载器会被预选并锁冲突；Fabric 下出现 Fabric API 勾选 |
| 安装向导 → 加载器 meta | Fabric 读取 BMCLAPI `fabric-meta`，Forge / NeoForge 读取 BMCLAPI Maven metadata；BMCLAPI 不可达时回退官方源；Fabric/Forge/NeoForge 分组都能显示版本 |
| 安装向导 → Forge | Forge 组显示 `64.1.5` 等短版本号；选中后默认名为 `26.1.2-Forge 64.1.5`，确认页显示 Forge；真实安装会用官方 installer 落盘 |
| 设置 Dialog 独立滚动 | 窗口高度压到 620px，设置内容滚到底时左侧三个分区导航位置不动（导航和内容各自 `overflow-y-auto`） |
| 左右栏拖拽 | 拖动两侧竖线改变左栏/右栏宽度；主区至少保留约 360px；刷新后宽度保持；折叠左栏时左侧分隔条消失 |
| 下载并发 | 滑块 1–256；首次越过 64 弹出“严重卡顿”提醒，关闭后仍可继续调到 256 |
| 下载并发 → 分段 | 并发 ≥ 2 时，超过 1 MiB 的文件按 HTTP Range 分段并发取（队列当前行显示「分段下载（N 个连接）」）。段数按空闲连接平均分，不再封顶 8 段；单段最小 64 KiB，最后一段可以更短。并发调成 1 后回到单流 |
| 只剩一个文件也别闲着 | 队列走到最后只剩 Fabric API 这类单文件时，空出来的连接会自动加派给它，界面上的「N/64 线程」要跟着涨到并发上限（默认 8 就是 8/8，设成 64 就是 64/64，文件不够切就用满能用的连接）。这个数字数的是真实连接数 |
| 阶段没有文件 | 装 Fabric 时「加载器」「安装器」「模组」一个文件都没有：这批下载跑完后它们显示「跳过」，不会一直挂着「等待」 |
| 下载重试 | 断网（或让 CDN 不可达）后点安装：任务不会一次就死，当前行依次显示「第 N 次重试（连接失败/传输中断）」；最终失败时错误是「连接超时（10060）：网络或代理不通（原始：…）」这样的人话，不再是裸 `os error 10060` |
| 服务器不支持分段 | 遇到忽略 `Range` 的地址装大文件：先试分段，发现回的是 200 就退回单流并成功，不会卡住，也不留 `.part` 残留 |
| 解析版本不再卡官方源 | 下载源选「镜像优先 / 仅镜像」后安装：队列很快离开「读取版本 JSON…」，不会先干等官方源超时；读 JSON 与读资源索引分成两步，客户端与依赖库在资源索引读完前就已经开始下载 |
| 下载源「自动（推荐）」 | 设置 · 游戏全局 → 下载源第一项是「自动（推荐）」，hint 说明"镜像只是把文件转回官方的就直连官方"；安装向导摘要显示「自动选源」 |
| 校园镜像兜底 | 设置 · 游戏全局 → 下载源下面一行「校园镜像兜底」，开关默认开；hint 列出 hit / nju / zju / ha / qlu / nyist / sjtu / ustc。下载源选「仅官方」时这一行消失（校园镜像属于镜像侧）；重新打开后 `settings.json` 里是 `"downloadCampusMirrors": true` |
| 校园镜像兜底 → 真的会换 | 把 `bmclapi2.bangbang93.com` 指向一个只会 404 的地址（`hosts` 到本机静态服务），装一个原版：队列当前行先显示「<文件名>：改用校园镜像 mirrors.hit.edu.cn」，文件能下完并校验通过；装的过程中不会卡在同一个 404 上 |
| 校园镜像兜底 → 队尾兜底 | 把主站和八家校园镜像一起指到 404：最终错误里带上真实的 HTTP 状态（不是「下载线程卡住了」），且任务可以重试 |
| 校园镜像不动第三方 | 装带 Fabric API 的 Fabric：Modrinth / CurseForge 的文件地址（`cdn.modrinth.com`、MCIM 镜像）不会出现在校园镜像主机上，只有 `/maven/` 与 `/assets/` 两类地址会被展开成校园地址 |
| `kaleid sources` 列出校园镜像 | 命令行跑 `kaleid sources`：先是 `mojang` / `bmclapi` 两个下载源（第三方清单插件装了才有第三个），末尾多一段「BMCLAPI 校园镜像兜底（文件下载失败时按顺序轮询，不提供清单）」，八行主机与基地址齐全 |
| 镜像被限流失效 | 把 `bmclapi2.bangbang93.com` 指到一个会回 200 + HTML 的地址（或用 `hosts` 指向本机静态服务）：应换到健康的官方源继续；全部候选都失败时，错误列出各地址及「返回的不是 JSON」原因。坏内容每个地址只请求一次，不反复撞错误页 |
| 安装失败不留垃圾 | 装到一半断网：报错后 `versions/<版本名>/` 不再出现在版本列表里（目录已被删干净）；`<数据目录>/staging/` 里也不留这次的 `forge-<job>` / `neoforge-<job>` |
| 正版登录状态 | 登录后把系统时间往后再调一天（或等够 24 小时）重启启动器：有效账号头像下显示「正版登录」，不是「登录已过期」；确实需要重新授权时显示「正版登录 · 需要重新登录」。有效会话点启动能正常进服 |
| 正版需要重新登录 | 在别处让这个账号的刷新令牌失效（改密码后等一会儿）：头像下变成「正版账号 · 需要重新登录」，出现「重新登录」按钮，点它走一遍设备码流程后恢复正常 |
| 版本列表自动刷新 | 安装跑完后不重开窗口、不手动点刷新，侧栏直接出现新版本并落在对应加载器分组；任务失败或取消也会重扫一次 |
| 版本列表手动刷新 | 版本区标题右侧的刷新按钮会重新调用 `versions_scan`，扫描期间按钮置灰并转圈，连点不会叠两次扫描 |
| NeoForge 版本读得出来 | 用带 `.neoforge/loader.json` 标记的版本（JSON 文件名与目录名不同也算）启动：侧栏把它们放进 NeoForge 组，显示加载器版本与对应原版；JSON 损坏但标记可读时仍列出，只是标记为未就绪 |
| 第三方清单安装 | `kaleid plugins install plugins/example-mirror --allow-unsigned` 打印落点并提示「下次启动生效」；重启后 `kaleid sources` 多出 `campus-mirror(prio=20)`，`mojang` / `bmclapi` 仍在；概览页底部出现只读说明卡片 |
| 第三方清单卸载 | `kaleid plugins remove example-mirror` 后重启：源与卡片一起消失，内置插件与内置源不受影响 |
| Fabric API 备用地址 | 装 Fabric 勾选 Fabric API：官方 `cdn.modrinth.com` 连不上时自动换 MCIM 镜像完成安装（两处地址来自同一条 `mirror_url_for` 规则） |
| NeoForge / Forge 安装器不再卡住 | 装 NeoForge（或 Forge）：队列走到「安装器」时要么字节条在动、要么那一行显示安装器自己吐出的最后一行日志（「… 安装器：Downloading libraries…」），15 分钟内一定收尾；万一内部状态出错，报的是「下载线程卡住了：没有任何可继续的任务（请重试）」这种能重试的错，而不是永远停在 0/1、0 线程 |
| 安装器阶段看得见 | 找个装得慢的 NeoForge 版本（或把 `maven.neoforged.net` 指到不存在的地址）：从点开队列到弹出「安装完成」，界面上一直在刷新安装器的输出，不会出现"进度条不动、也不知道它是不是死了"的那十几分钟 |
| APP 设置 → 界面字体 | 默认跟随系统；下拉可搜索系统字体、显示真实系统 UI 字体；可导入 ttf/otf/ttc，应用后全局字体改变 |
| APP 设置 → 界面字号 | 「−」「+」每次 1 px（范围 11–20），中间实时显示当前 px；改动立刻生效——文字、行距、按钮一起**等比**缩放（等同浏览器缩放），不会出现「字大了但行高没变」的错位；到 11 / 20 时对应按钮变灰；不是默认值时出现「恢复默认」回到 13 px |
| 界面字号 → 持久化 | 调到 16 px 后刷新页面、重开窗口都还是 16 px（存在 `kaleid.prefs.v1`）；数据目录里的 `settings.json` 不因此变化（界面偏好不进游戏设置） |
| 界面字号 → 极值布局 | 在 20 px 下设置 Dialog、安装向导、右栏新闻与下载队列仍完整可用：没有被裁切，也没有突然出现的横向滚动条；弹窗的上下边都在窗口里（`max-h` 已按缩放折算） |
| 文件夹「…」菜单 | 鼠标移到文件夹行上出现「…」，点开菜单**完整浮在主区之上**（不是只有侧栏那一条），四项齐全；字号调到 20 px 时菜单位置仍然贴着按钮、整块在窗口内；菜单打开后点别处能关掉 |
| 下拉弹层（下载 Mod 的「来源」） | 列表贴着按钮下方、宽度与按钮一致、不会被 Dialog 裁掉；靠近窗口底边时向上翻 |
| 界面字号 → 拖分隔条 | 字号 20 px 时拖左右竖线，光标移动多少、栏宽就变多少（不会跑得比鼠标快） |
| 停用「模组下载」→ 全局设置 | 「Mod 数据源」「Mod 列表注释」两行消失；「下载并发」保留（版本安装还在用同一个线程池）；插件页重新启用后两行回来 |
| 版本管理 + 模组下载都停用 | 游戏全局设置只剩内存 / Java / 启动前检查 / 默认版本隔离四行，最后一行底下没有多余分隔线 |
| 登录卡头像 | 未登录/离线都是史蒂夫；`skinModel: slim` 的账号显示亚历克斯；换账号时头像淡入 |
| 许可信息 | 仓库根目录有 `LICENSE`（Apache-2.0 全文，附录已填 `Copyright 2026 Sky_Dynamic`）与 `NOTICE`；`Cargo.toml`、`src-tauri/Cargo.toml`、`ui/package.json` 的 license 字段都是 `Apache-2.0` |
| 模组页 →「下载 Mod」 | 按钮在搜索框右边；没选版本时 disabled，`title` 为「先选择一个版本」 |
| 下载 Mod → 首屏 | 直接显示下载量排行（Modrinth + CurseForge 合并降序）；再次开关 Dialog **不会**重新请求（本次启动只取一次） |
| 下载 Mod → 刷新 | 强制绕过服务端缓存重新取元数据（有搜索词时重跑搜索，没有时重取排行） |
| 下载 Mod → 搜索 | 名称 + 来源（全部/Modrinth/CurseForge）+ 类型（按大类分组的下拉）；回车等同点「搜索」；「重置条件」清空并回到排行 |
| 下载 Mod → 列表项 | 左 icon（8px 圆角）；第一行译名 `|` 原名（原名灰体）；第二行类型 chip + 截断简介；第三行 加载器+版本区间 / 下载量（亿·万）/ 最后更新 / 来源，各带图标 |
| 下载 Mod → 单源失败 | 顶部黄色提示条说明原因（例如「Modrinth 没有「世界生成」分类…」），另一源的结果照常显示 |
| 下载 Mod → 详情 | 点行进入详情：icon + 译名/原名 + 简介（MCIM 译文）+ 加载器/区间/下载量/更新时间；每个「加载器 + MC 版本」一组，可展开 |
| 下载 Mod → 前置 | 展开的组若需要前置，先显示「前置资源」列表（样式与搜索列表一致，可点进去），下方才是「版本文件」列表 |
| 下载 Mod → 安装确认 | 点版本文件弹第二层 Dialog：显示安装目标版本/目录、文件版本、大小、发布类型；已装过同名文件时出现提醒与「允许覆盖」勾选 |
| 下载 Mod → 真机安装 | 确认后进入下载队列（来源显示 MCIM），文件落在 `<游戏目录>/mods`；内容相同的文件直接判定为已装好 |
| 设置 → 下载源 | 四档：官方优先 / 镜像优先 / 仅官方 / 仅镜像；选完刷新队列，任务的来源标签与提示（例如「官方源较慢，已切换 BMCLAPI」）只在「官方优先」下出现 |
| 设置 → Mod 数据源 | 官方优先 / 镜像优先 / 仅官方；「仅官方」时提示 CurseForge 仍需 API Key，因此 CF 走镜像 |
| 设置 → Mod 列表注释 | 原文 / 翻译，默认原文；切成翻译后列表简介来自 MCIM（数据源选「仅官方」也一样），拿不到译文时回落原文并给出提示 |
| 旧配置升级 | 老 settings.json 里 uto / mojang / mclapi 与 uto / mcim / official 会被折算成新取值，设置页不会出现「选不中任何一项」 |
| 停用「模组下载」插件 | 模组页按钮消失；已打开的下载 Dialog 自动关闭；`mods_*` 命令返回「模组下载服务不可用（插件可能已停用）」 |
| 模组页 → 真机列表 | 显示**真实** `mods/` 里的文件（不再是演示数据）；勾选框对应 `.jar` / `.jar.disabled`，点一下真的会改名 |
| 模组页 → 导入 | 「导入」打开原生多选（只列 .jar）；同名文件被跳过并提示「同名文件已存在（不覆盖）」 |
| 模组页 → 拖入 | 从资源管理器拖 .jar 到窗口上，文件被复制进 `mods/`；拖非 jar 提示「只能拖入 .jar 模组文件」 |
| 模组页 → 删除失败 | 游戏运行中删除被占用的 jar：弹出「删除模组失败」Dialog 并显示系统原因，不是一闪而过的 toast |
| 资源包页 | 启用状态来自 `options.txt`，勾选框**不可点**并标注来源；导入/删除/打开目录可用 |
| 资源包页 → 导入后 | `options.txt` 的字节**没变**（启动器只读不写） |
| 存档页 | 显示 world 名字、游戏模式、MC 版本、最后游玩时间、大小；超大世界显示「≥」 |
| 存档的最后游玩时间 | `level.dat` 里 `LastPlayed` 是**毫秒**，界面按**秒**解释会显示成 5xxxx 年；现在后端换算成秒，界面兜底，正常应显示 `2025/10/27 04:14` 这种 `YYYY/MM/DD HH:mm` |
| 日志页 | 列出 `logs/` 下全部文件（含 `.log.gz`），点一个在右侧预览尾部；被截断时顶部写「只显示最后 N KB」 |
| 日志页只滚内容框 | 打开几十万行的日志滚动：只有左侧文件列表与右侧正文两个框滚，页签和工具栏不动；把窗口拉矮，正文框跟着变矮而不是把整页顶出去 |
| 日志着色 | `[main/INFO]` 的级别绿、`WARN` 琥珀、`ERROR`/`FATAL` 红；`WARN`/`ERROR` 行有对应颜色的左边线与淡淡底色；时间戳、线程名、`[minecraft/Xxx/]` 这类 logger 名淡显 |
| 日志堆栈 | `\tat …` / `Caused by:` / `… 12 more` 缩进淡显，且**不计入**右上角的错误/警告计数 |
| 日志 ANSI | 带 `\u001b[31m` 之类转义的日志按色渲染（红/绿/黄/蓝/青/品红），加粗生效，控制码本身不外露；「复制」拿到的仍是原文 |
| 日志乱格式 | 没有 logger 名的 `[12:00:10] [Render thread/INFO] 消息`、纯 `[INFO] 消息`、完全无级别的行都照常显示，不吞字符 |
| 日志统计徽标 | 顶部显示「错误 N」「警告 N」（0 时不显示）；切到别的日志后跟着更新 |
| 目录跟随隔离 | 游戏设置里把版本隔离切成「开启/关闭」，模组/资源包/存档/日志的路径与主区顶部的 `gameDir` **同时**变化 |
| 游戏设置 → Java 自动 | 显示真的挑中的运行时（名称 + 来源），换版本后跟着版本要求的 Java 主版本变化 |
| 游戏设置 → Java 手动 | 下方列出扫到的 Java（单选行显示版本/来源/路径）；「浏览文件夹…」选一个 JDK 后写回设置；选到不像 Java 的目录当场提示 |
| 「校验」按钮 | 通过时提示资源文件总数；不通过时弹 Dialog 分项列出缺 jar / 缺依赖库 / 缺索引 / 缺资源文件 |
| 校验只看根目录 | 把 `libraries/` 藏到 `versions/<id>/libraries` 后点校验：仍然报缺依赖库 |
| 正版登录 → 弹窗 | 概览「添加正版账号」弹出对话框：**系统浏览器自动打开** `microsoft.com/link`，弹窗里是 8 位代码（等宽大字号）+「复制」+ 倒计时（从 15:00 递减） |
| 正版登录 → 轮询节奏 | 不进浏览器确认：弹窗一直停在"正在等待…"，每 **10 秒**才发一次 `msa_poll`（开发者工具 Network 里数得出节奏） |
| 正版登录 → 真机 | 浏览器里输入代码并登录后，弹窗自动关闭，账号出现在登录卡里、头像为该账号真实皮肤 |
| 正版登录 → 取消 | 关掉弹窗后 `msa_poll` 再被调用会报「还没有发起登录」，说明 `msa_cancel` 真的清了设备码 |
| 正版登录 → client_id 没配 | 把 build.env 的 `KALEID_MSA_CLIENT_ID` 留空重编译：按钮 disabled 且下方提示「还没配置 client_id（build.env 的 KALEID_MSA_CLIENT_ID）」 |
| 正版登录 → client_id 填错 | 填一个不存在的 GUID 重编译：弹窗显示 `AADSTS700016` 并指出「client_id 打错」或「应用未开放个人帐户/公共客户端流」两条路 |
| 正版登录 → 应用未获授权 | 用未申请 Minecraft API 权限的自建应用登录：`login_with_xbox` 返回 403，弹窗显示「不是令牌的问题…去 https://aka.ms/mce-reviewappid 申请」，并附官方原文 `Invalid app registration` |
| 正版登录 → XErr | 用没有 Xbox 档案的账号登录：报「先在 xbox.com 登录一次创建档案」而不是「XSTS 失败」 |
| 正版登录 → 令牌落盘 | 登录后 `<数据目录>/msa-tokens/<uuid>.bin` 存在且用记事本打开是乱码；`accounts.json` 里搜不到 `eyJ` / `refresh` / `access_token` |
| 正版登录 → 过期提示 | 手动删掉 `msa-tokens` 里的文件后重启：登录卡副标题变成「正版账号 · 登录已过期」，并提示启动时会用刷新令牌续期 |
| 微软登录是必需插件 | 插件页「微软登录」带「必需」chip、`Switch` 置灰，悬停提示「正版账号登录由它提供，停用后无法登录微软账号，只能离线启动」 |
| Java 运行时 / 本地内容是必需插件 | 两行都带「必需」chip、`Switch` 置灰，悬停分别提示「挑不出该用哪个 Java，游戏启动不了」「版本页四个标签全空」 |
| 必需插件后端拒绝停用 | 直接调 `plugins_set_enabled {id:'java-runtime', enabled:false}` 返回「插件 `java-runtime` 是必需插件，不能停用」，且插件状态仍是 `ready`（被拒绝的停用不能先拆服务再报错） |
| 补丁不能停必需插件 | `presets` 里用 `disable` 或 `update: enabled: false` 指向必需插件，装载直接失败并说明原因；停可选插件（如 `version-manager`）照旧生效 |
| 模组页 → 图标列 | 有图标的模组在最前面显示 18px 圆角图标（`content_mod_icon` 按需取，不是一次性全拉）；没有图标的显示统一占位纹理，停用的模组图标变淡 |
| 模组页 → 真实 meta | 名称/版本/作者来自 jar 内的 `fabric.mod.json` / `neoforge.mods.toml`；坏 jar 不报错，回落文件名（`meta_source == "filename"`） |
| 模组页 → 大 jar 的 meta | 条目两万多个的 jar（如 `decocraft-3.0.7-1.21.1-fabric.jar`，里面同时有展开好的 `fabric.mod.json` 和一份没展开的 `META-INF/mods.toml` 模板）显示的是真名 `Decocraft` / `3.0.7`，不是 `${mod_name}` / `${mod_version}` |
| 模组页 → 只剩模板 | jar 里只有未展开的 `mods.toml`（全是 `${mod_name}` 之类）时，不显示占位符：跳过模板用 MANIFEST，再不行回落文件名 |
| 模组页 → 更新检查 | 进入页面即开始异步检查（顶部「正在检查模组更新…」），有新版本时出现提示条「N 个模组有更新」+「全部更新」/「忽略」；单个模组行出现「更新」按钮 |
| 模组页 → 应用更新 | 点「更新」后新文件落进 `mods/`，旧文件被移到 `mods/.kaleid-backup/<时间戳>/`（不是直接删）；新文件的启用态跟随旧文件 |
| 模组页 → 更新查不到 | 断网或 CurseForge 无 Key 时只在报告里记 `unknown`/`warnings`，列表照常可用，不弹错误 |
| 下载 Mod → 中文搜索 | 搜索框输入「钠」/「锂」这类中文名，能搜到对应项目（后端先用名称表翻成 slug 再查 Modrinth）；翻不出来时退回原词而不是报错 |
| 下载 Mod → 首屏排行 | 首屏按下载量取**一页 50 个**，facets 带上 `project_type:mod` + 当前游戏版本 + 当前加载器（有类型时再加 `categories:<类型>`）；同一组参数 10 分钟内再进不重新请求 |
| 版本行「…」菜单 | 鼠标移到侧栏版本行出现「…」：更新加载器…（原版版本点它只提示"没有安装模组加载器"）、打开版本设置、删除版本 |
| 删除版本 | 弹确认框（说明会连版本设置一起删、存档模组不跟着删）；确认后版本从侧栏消失，被继承的版本拒绝删除并提示原因 |
| 更新加载器 | 弹窗先查 `versions_loader_status`：显示加载器、已安装版本、最新版本（有更新时带「有更新」chip）、可选目标版本下拉；确认后进入下载队列替换「版本目录自包含」那一层，客户端 jar 与资源不动 |
| 启动游戏 | 点「启动游戏」后按钮持续 loading（准备中 → 启动中），直到游戏窗口出现才恢复；运行中再点会提示「游戏已经在运行」 |
| 启动游戏 → 标识 | 游戏内左下角显示标识（`--versionType` + `-Dminecraft.launcher.brand`）；在游戏设置里停用标识或清空文字后启动，参数里不再有 `--versionType` |
| 启动游戏后操作 | 设置里四档：保持不变 / 最小化 / 隐藏至托盘 / 退出启动器。选「最小化」启动后窗口收进任务栏；选「隐藏至托盘」窗口消失、**游戏退出后启动器窗口自己回来**；选「退出」启动器退出但游戏继续跑 |
| 崩溃 → 弹窗 | 游戏非零退出（或日志出现崩溃标记）时弹「游戏崩溃了」：关键词标签带命中次数、疑似模组按「高度可能/可能/推测」排序并附原始日志行、崩溃前 400 行日志可滚动复制 |
| 崩溃 → 导出 | 「导出排障包」在数据目录 `crash-exports/<版本>-<时间>/` 下写出 `latest.log`、`last-log-before-crash.txt`、`crash.txt`（有官方报告时）、`launch-script.txt`、`manifest.json`，导出后可直接「打开目录」 |
| 启动脚本不泄令牌 | 导出的 `launch-script.txt` 里 `--accessToken` 的值是 `***`；`manifest.json` 也不含令牌 |
| 存档页 → 删除 | 版本行「删除」要连点两次（第一次变红提示"再点一次确认"），删完列表少一个存档并提示释放多少空间 |
| 存档页 → 数据包 | 点数据包图标打开弹窗：列出 zip 与目录包、显示 `pack_format`、简介、大小；启用状态标注「已启用/未启用/未知」并说明**只读**（来自 `level.dat`） |
| 存档页 → 数据包导入 | 「导入 zip」走原生多选，重名落盘成 `name-1.zip`/`name-2.zip`；导入后列表立刻刷新 |
| 展开动画 | 侧栏版本分组、安装向导的四组版本列表、下载 Mod 的文件分组、设置分区都是高度过渡展开/收起（`0fr → 1fr`），开关动效或系统「减少动态效果」时动画归零 |
| 安装向导 → OptiFine 分组 | 第二步原版 / Fabric / Forge / NeoForge 下面多出「OptiFine」一组，能读出 `HD_U_*` 版本（`pre` 版标「预览版」、稳定版标「正式版」）；选一个后 Fabric 与 NeoForge 立刻变灰，OptiFine 卡片下方写明「会放进 mods/」还是「作为独立版本安装」 |
| 安装向导 → OptiFine 与 Forge | 高版本（如 `26.1.2`）点 OptiFine 版本：加载器**自动切到 Forge** 并挑推荐版本，确认页多一行 `OptiFine HD_U_J3（放进 mods/，配合 Forge）`；再把加载器改回原版时「下一步」变灰并提示「必须和 Forge 一起安装」 |
| 安装向导 → OptiFine 与 Fabric 互斥 | 先选 Fabric 再展开 OptiFine：整组灰掉并提示「Fabric / NeoForge 不支持 OptiFine」；先选 OptiFine 再点 Fabric 版本：OptiFine 的选择被清掉，确认页那一行随之消失 |
| 概览 → 离线名用中文 | 输入「小明」：名字保存生效、头像照常；输入框下方出现琥珀色警告「原则上Minecraft任何版本都不接受使用中文ID，若出现问题后果自负」；改回 `Player_123` 后警告消失；输「名字 空格」「ひらがな」「名字!」当场报红且不改动当前名字 |

> 新闻仍是界面层直接 `fetch` 官方 feed（`ui/src/store/news.ts`），内置 `news`
> 插件只负责默认开启与槽位开关；宿主另有版本清单/皮肤解析的 HTTP 依赖。
> 这条取舍写在设计规格的「已知取舍」里。

### 6.5 这一轮 8 项需求的手工验收

| 需求 | 操作 | 期望 |
|---|---|---|
| ① Mod 初始列表 | 打开「下载 Mod」，不输入任何检索词、不改筛选 | 直接出 Modrinth 排行（与点「搜索」空条件的结果一致），每页 50 条，底部显示「上一页 / 第 1 / N 页 · 每页 50 / 下一页」；**不再**出现「这个筛选条件下 Modrinth 没有可用的模组」 |
| ① 回到顶部 | 列表滚过 60% | 右下角浮出圆形「回到顶部」按钮，点一下平滑回顶；回到顶部后按钮消失；点开某个 Mod 的详情页时按钮隐藏 |
| ② 多开 | 启动一个版本，再点顶部按钮（此时显示「再启动一个」） | 第二个游戏进程照常起来；两个进程互不影响，各自有独立日志目录；**没有**「结束游戏」按钮 |
| ② 登录失败 | 用一个令牌已失效的微软账号启动 | 弹出「无法启动」对话框，写明原因（例如「微软没有受理这次令牌刷新…请重新登录这个账号」），提供「去登录」；游戏**不会**启动 |
| ③ 不再问目录 | 数据目录里有两个以上文件夹时启动应用 | 直接进主界面，不弹选择框；需要换目录时用左栏文件夹菜单 →「切换游戏目录…」（标题「切换游戏目录」，勾「记住为默认」才写入默认值） |
| ④ 无 mock | 打包版（非 `dev`）里操作任何页面 | 所有数据来自 Rust 宿主；桥接缺失时显示明确的错误提示，不会静默退回示例数据；`pnpm --dir ui dev` 的浏览器预览底部才有「浏览器演示模式」琥珀条 |
| ⑤ 默认 JVM 参数 | 打开 全局设置 →「高级设置」，再启动任意版本看 `logs/<版本>-<时间>-<会话>/launch-script.txt` | 输入框里**直接就是**那四条（`-XX:-OmitStackTraceInFastThrow`、`-Djdk.lang.Process.allowAmbiguousCommands=True`、`-Dfml.ignoreInvalidMinecraftCertificates=True`、`-Dfml.ignorePatchDiscrepancies=True`），命令行里也含它们；改一条 → 命令行跟着变；删一条 → 命令行里也没有；点「恢复默认」→ 四条回来 |
| ⑤ 老配置迁移 | 手写一份 `settings.json`，里面是 `"jvmArgs": []`，启动后打开高级设置 | 输入框自动补成那四条（补完立刻落盘，带 `jvmArgsInitialized: true`）；此后手动清空 → 重启仍是空，不会被"默认"顶回来 |
| ⑥ 版本级参数 | 游戏设置 →「高级设置」→ 参数方式「追加到全局」+ 写一条参数 | 命令行里全局参数在前、本版本参数在后；切成「覆盖全局」后全局那几条消失；卡片标题旁实时显示当前模式，切页面回来展开状态保持 |
| ⑥ 自动中文 | 全局设置 →「高级设置」，「自动将游戏调为中文」**默认就是开的**，启动任意版本 | 该版本 `game_dir/options.txt` 的 `lang:` 变成 `zh_cn`（1.10 及更早为 `zh_CN`）：只动这一行、CRLF 文件仍是 CRLF、其它行一字不改；装 YOSBR 时改的是 `config/yosbr/options.txt`；启动状态的说明里会写清"从 en_us 改成 zh_cn"；关掉开关后不再改 |
| ⑦ 全局高级设置卡 | 游戏全局设置 | 底部有可折叠「高级设置」，展开后是 JVM 参数（预填默认四条 + 「恢复默认」/「清空」）/ 内存管理（尽量使用 ZGC、尽量使用分代 ZGC、标准 G1GC、调优 G1GC、不指定）/ 使用高性能显卡 / 自动将游戏调为中文 / 保存 LatestLaunch.bat |
| ⑦ GC 档位 | 选「调优 G1GC」，启动 | 命令行里出现 `-XX:+UseG1GC`、`-XX:+UnlockExperimentalVMOptions`（必须排在 `-XX:G1NewSizePercent` 之前，否则 JVM 拒绝启动）、`-XX:G1HeapRegionSize=32M`、`-XX:+PerfDisableSharedMem`；换成「尽量使用分代 ZGC」但 Java 只有 17 时，自动降级并在启动状态 `notes` 里说明原因 |
| ⑦ 内存管理默认档 | 全新数据目录启动，打开 全局设置 →「高级设置」 | 下拉框停在**标准 G1GC**；启动命令行里是 `-XX:+UseG1GC -XX:G1NewSizePercent=20 -XX:G1ReservePercent=20 -XX:MaxGCPauseMillis=50`（没有 `-XX:+UseZGC`） |
| ⑦ 高性能显卡 | 全新数据目录里它**默认是开的**；启动一次游戏 | `HKCU\Software\Microsoft\DirectX\UserGpuPreferences` 下出现以启动器 exe 完整路径为名、值为 `GpuPreference=2;` 的字符串；`javaw.exe` 也有一条；关掉开关后启动器那条被删除；Linux / macOS 上开关是禁用状态、显示「当前系统不支持」，存着的 `true` 也不产生任何注册表写入 |
| ⑧ LatestLaunch.bat | 启动任意版本 | `<数据目录>/LatestLaunch.bat` 出现（默认开启），内容是 `chcp 65001` + `title 启动 - <版本>` + `cd /D` + 完整 Java 命令行，访问令牌是 `***`，结尾 `echo 游戏已退出。` + `pause`；崩溃导出里也带上它 |

> 上面几条里只有 ⑦ 的高性能显卡和 ② 的多开需要真机（注册表/进程）才能验收；
> 其余都能在 `pnpm --dir ui dev` + 真实宿主里点一遍。

### 6.6 「什么版本都起不来」的排查链

真机上踩到过一次：从 PCL / HMCL 接手过来的 `.minecraft`，点启动后 200 毫秒退出、
`exit code 1`，日志里只有 `Picked up JAVA_TOOL_OPTIONS`，一行 MC 的都没有。三个独立
原因串在一起，全都出在「按 JSON 拼命令行」这一段：

| 现象 | 根因 | 修法 |
|---|---|---|
| `ClassNotFoundException: net.fabricmc.loader.impl.launch.knot.KnotClient` | 加载器 profile 里的库只写了 Maven 坐标 + `url`，没有 `downloads.artifact`，被 `continue` 丢掉 | `install::library_relative_path`：没有现成路径就按坐标推 `group/artifact/version/artifact-version.jar` |
| `duplicate ASM classes found on classpath` | 原版的 `asm:9.6` 和加载器的 `asm:9.9` 同时进了 classpath | 按 `group:artifact[:classifier]`（`artifact_key`）去重，后出现的覆盖先出现的 |
| `--xuid` 后面直接跟 `--versionType` | 空值参数被 `.filter(!is_empty)` 吃掉，命令行错位 | 空串也是合法取值，保留；`command_line()` 把它印成 `""` |
| 界面每个版本都显示「缺 24–43 文件」 | 完整性检查同样只认 `downloads.artifact`，还把别的系统的 natives 当缺失 | 复用同一套路径推导 + `rules_allow` 过滤，并认 `versions/<id>/libraries/` |
| NeoForge 版本被认成 Fabric | NeoForge 自己依赖 `net.fabricmc:sponge-mixin`，`classify` 看到 `net.fabricmc` 就判 Fabric | 先判 NeoForge / Forge，Fabric 只认 `fabric-loader` |

验收方式（都是真机跑过的）：

```powershell
# 12 个版本全部「就绪」
.\target\debug\kaleid.exe --data-dir "$env:LOCALAPPDATA\Kaleid" versions
# 把 plan 打印出来的命令行喂给真 java，看它能不能起来（javaw 换成 java 才有输出）
.\target\debug\kaleid.exe --data-dir "$env:LOCALAPPDATA\Kaleid" plan "FriendServer-1.21.10-All-online"
```

### 6.7 PCL2 式启动流程的手工验收

自动化测试覆盖了状态机、ZIP 写入、缺失清单和参数解析；下面这些必须真机点一遍
（自动化没法验证"窗口真的出现了"和"系统目录选择器弹出来了"）。

**A. 进度覆盖层 + 自动补全（最该验的一条）**

先人为弄缺两个资源对象（拿版本自己用的资源索引里的 hash，别随手删）：

```powershell
$root = 'D:\MC\.minecraft'
$v = Get-Content "$root\versions\FriendServer-1.21.10-All-online\FriendServer-1.21.10-All-online.json" -Raw | ConvertFrom-Json
$idx = Get-Content "$root\assets\indexes\$($v.assetIndex.id).json" -Raw | ConvertFrom-Json
$idx.objects.PSObject.Properties | Select-Object -First 2 | ForEach-Object {
  Remove-Item ("$root\assets\objects\" + $_.Value.hash.Substring(0,2) + "\" + $_.Value.hash)
}
```

然后启动那个版本，期望依次看到：

1. 覆盖层弹出，「检查启动条件」打勾 → 「补全缺失文件」转圈，右侧出现
   `xx KB / xx KB · 1 / 2 个文件`；
2. 补完显示「已补全 2 个文件」→ natives、启动进程（这两步很快）；
3. 「等待游戏窗口」最长十几秒，然后覆盖层自己收起、右上角按钮变「再启动一个」；
4. 游戏里能看到完整画面（少资源的话会缺材质，这就说明补全真的生效了）。

命令行等价物（不点界面也能验，会一直跟到游戏退出）：

```powershell
.\target\debug\kaleid.exe --data-dir "$env:LOCALAPPDATA\Kaleid" launch "FriendServer-1.21.10-All-online"
```

**B. 取消启动**

删掉一个较大的资源对象（几百 KB 以上）再启动，在「补全缺失文件」阶段点「取消启动」：
覆盖层变成「已取消启动」+「知道了」，任务栏里不该出现新的 javaw 进程。
`--cancel-after 300` 是同一个路径的命令行版本。

**C. 运行中实例面板（概览页）**

1. 启动后回概览，面板出现一行：版本名、`#会话 · PID`、运行时长，右侧三个按钮；
2. 「日志」→ 弹窗里有这次的 stdout（含 `Kaleid` 自己的启动行），勾着「跟随最新输出」
   时每秒刷新、自动滚到底；
3. 「导出」→ 弹系统目录选择器（默认落到上次选的目录），确定后 toast 显示
   `已导出 崩溃日志-YYYY_MM_DD HH-MM-SS.zip（N 个文件，x.x MB）`；
   打开 zip 应有 `latest.log`、`last-log-before-crash.txt`、`launch-script.txt`、
   `LatestLaunch.bat`、`manifest.json`（崩过的话还有 `crash.txt`）；
4. 「结束进程」→ 行状态变「已退出 · 退出码 1」，游戏窗口消失。

**D. 崩溃仍然弹报告**

沿用 6.4 的做法（装一个必定让游戏崩的模组），确认：进过游戏之后崩溃，弹的是
**崩溃报告弹窗**，覆盖层不会说「启动失败」；而窗口都没出现就退出的情况（比如
Java 路径被改坏）才会显示启动失败 + 具体原因。

### 6.8 退出和崩溃怎么区分

真机上踩到过：正常点「退出游戏」、或者直接点窗口右上角的 X，启动器却弹崩溃报告。
根因是判定把「关键词命中」当成了崩溃证据，而 `graphics` 规则的 needle 里有 `opengl`
这个词——每局启动都要打 `OpenGL Vendor / Renderer / Version` 好几行，于是**每一局
都是"崩溃"**（实测一次正常退出的日志里 `opengl` 出现 7 次）。

现在的判定只看证据，**关键词只用来提示"哪一类问题"**：

| 判为崩溃的条件 | 说明 |
|---|---|
| 这次会话真的落下 `crash-reports/*.txt` | 按会话开始时间过滤，上一次的旧报告不算 |
| 日志里有致命标记 | `---- Minecraft Crash Report ----`、`A fatal error has been detected`、`Exception in thread "main"` 等 |
| 非零退出码 **且** 没走关服流程 | 打出 `Stopping!` / `Save queue flushed!` 就说明是正常退出 |
| 例外：用户点「结束进程」 | 主动结束必然非零退出码，但那是用户要求的，不算崩溃 |

对应地，关键词规则里那些"日常噪音"也收紧了：`opengl`、`registryaccess` 这类词
（正常日志、堆栈里到处都有）不再作为线索。

验收（都做过）：

```powershell
# 1) 拿这台机器上一次真实启动日志回放判定，应当 detected = false
$env:KALEID_REAL_LOG = "$env:LOCALAPPDATA\Kaleid\logs\<某个会话>\console.log"
cargo test -p kaleid-core-plugins --lib crash::tests::real_log_is_judged_by_evidence -- --ignored --nocapture

# 2) 真机：启动游戏，等 ✅ 已启动，然后优雅关窗（不带 /F 的 taskkill 等于点 X）
.\target\debug\kaleid.exe --data-dir "$env:LOCALAPPDATA\Kaleid" launch "FriendServer-1.21.10-All-online"
taskkill /PID <上面打印的 pid>     # 期望输出：游戏已退出（退出码 Some(0)），不是崩溃
```

### 6.9 这一轮 4 项需求的手工验收

| 需求 | 操作 | 期望 |
|---|---|---|
| ① 低版本能启动 | 装一个 `1.12.2`（或 `1.7.10`）原版，启动 | 不再出现 `no lwjgl in java.library.path`；`versions/1.12.2/natives/` 里能看到 `lwjgl.dll` / `lwjgl64.dll` / `OpenAL64.dll` 等解压出来的文件；启动脚本里 `-Djava.library.path` 指向这个目录；换成 1.21.4 启动，两边的 natives 目录**互不覆盖** |
| ① 老版本命令行没有占位符 | 装 `1.7.10` 后看 `logs/<版本>-<时间>-<会话>/launch-script.txt` | 没有残留的 `${auth_session}` / `${user_properties}`（分别是 `0` 和 `{}`）；客户端不再因为 `user_properties` 解析失败而闪退 |
| ② NeoForge 装得上 | 设置 → 下载选「官方优先」，装 `26.1.2` + NeoForge `26.1.2.125`；官方 Maven 不通也没关系 | 队列不会卡在「安装器」；如果下载到的确实是 HTML 错误页，报错是「安装器不是有效的 jar…请到「设置 → 下载」换一个下载源（推荐「BMCLAPI 优先」）后重试」，而不是一句「安装器失败」；换「BMCLAPI 优先」后能装完并出现启动按钮 |
| ③ OptiFine 装得上（高版本） | 装 `26.1.2` + Forge `64.1.5` + OptiFine `HD_U_J3` | `mods/` 下出现 `OptiFine_26.1.2_HD_U_J3.jar`；版本 JSON 里 `kaleidOptifine.mode == "mod"`；启动后在游戏里能打开 OptiFine 设置（视频设置里多出 OptiFine 页） |
| ③ OptiFine 装得上（老版本） | 装 `1.12.2` 原版 + OptiFine `HD_U_G5`（不选 Forge） | 版本 JSON 的 `mainClass` 是 `net.minecraft.launchwrapper.Launch`，`minecraftArguments` 末尾有 `--tweakClass optifine.OptiFineTweaker`，`libraries` 里有 OptiFine 本体、`launchwrapper:1.12`、`asm-all:5.2`；侧栏该版本落在 **OptiFine** 分组；启动后游戏正常进主界面 |
| ③ OptiFine 的安全网 | 尝试「`1.21.4` + Fabric + OptiFine」「`1.21.4` 原版 + OptiFine（不选 Forge）」两组合 | 界面就不让选：Fabric / NeoForge 组变灰；原版 + OptiFine 时「下一步」禁用并提示要先选 Forge；若绕过界面直接调 `versions_install`，后端返回「OptiFine 只能和 Forge 一起安装」或「…必须和 Forge 一起安装」 |
| ④ 中文离线名 | 概览 → 离线登录，输入「小明」 | 立即生效（停笔 300 ms 后落盘），`accounts.json` 里名字是「小明」，uuid 与 `OfflinePlayer:小明` 的 Java 算法一致；输入框下方出现琥珀色警告「原则上Minecraft任何版本都不接受使用中文ID，若出现问题后果自负」；进单人游戏名字显示正常（进正版服务器可能被踢，属预期） |
| ④ 中文名不越界 | 输入「名字 空格」「ひらがな」「名字!」「玩」×17 | 全部当场报红、当前名字不变；提示语是「只能用 1–16 位字母、数字、下划线或汉字」 |

### 6.10 这一轮 2 项需求的手工验收（下载速度 / 登录保持）

| 需求 | 操作 | 期望 |
|---|---|---|
| ① 下载源「自动」 | 设置 · 游戏全局 → 下载源选「自动（推荐）」，装 `1.21.4` 原版 | 版本 JSON 与资源索引走镜像；客户端 jar 与资源文件不再因为镜像只是转回官方而多绕一跳；队列里不再出现成片的「第 N 次重试（HTTP 429）」 |
| ① 大文件跑满带宽 | 装 `1.21.4 + Fabric`，并发保持默认 8 | 大文件（client.jar、资源索引）按 `Range` 分段，队列行显示「分段下载（N 个连接）」；右上角线程数一直在 6–8 之间，不再出现"1/8 慢慢爬" |
| ① 镜像被限流时换源 | 把 `bmclapi2.bangbang93.com` 限到每分钟 1 次（或 `hosts` 指到本机静态服务） | 429 先按 `Retry-After` 等够再试同一地址；连续被限流才整体切到官方源，切换后进度继续涨，不会掉回 0 |
| ① 镜像回 HTML 错误页 | 同上，让镜像返回 200 + `text/html` | 报错是「… 返回的不是 JSON（Content-Type: text/html）：<…>（已尝试 2 轮，地址：…）」，不会出现 `expected value at line 1 column 1` |
| ① 失败要收拾干净 | 装到一半断网，然后恢复网络再装一次 | 失败后版本列表里没有那个半成品版本，`versions/<名字>/` 已被删除，`<数据目录>/staging/` 不再堆积 `forge-<job>` 目录；再装一次能正常完成 |
| ② 登录状态保持 | 正版登录后关掉启动器，把系统时间往后调一天（或真的等 24 小时）再打开 | 头像下是「正版账号 · 登录有效 · 自动续期」，不是「登录已过期」；点启动能进服，期间刷新令牌已被轮换并重新加密落盘 |
| ② 续期先落盘 | 断网状态下点启动（此时访问令牌已过期） | 报的是「登录状态已续期，但换取 Minecraft 令牌失败：…」这类可重试的错误；恢复网络后直接点启动即可成功，**不需要**重新登录 |
| ② 真掉登录才提示 | 在 accounts.microsoft.com 改一次密码（会让旧刷新令牌失效），等几分钟后打开启动器 | 头像下变成「正版账号 · 需要重新登录」，并出现「重新登录」按钮；点它走完设备码流程后恢复成「登录有效」 |


## 7. 可能会撞到的问题

- **依赖下载慢**：`.cargo/config.toml` 已把 crates.io 换成 rsproxy 镜像。
- **`pnpm --dir ui install` 退出码 1**：pnpm 11 默认拦 esbuild 的 postinstall，
  `ui/pnpm-workspace.yaml` 里已用 `allowBuilds: esbuild: true` 放行。
- **`cargo build --release` 出来的程序打开是白屏**：检查是否误改了
  `src-tauri/Cargo.toml` 的 `default = ["tauri/custom-protocol"]`。
