# JVM 参数与 GC 档位的出处

这一页回答两个问题：**启动器默认加了什么**，以及**那几个 GC 档位是谁的参数**。
数值不是拍脑袋定的，每条都指回来源。

## 1. 默认参数（预填在设置里，可改可删）

```text
-XX:-OmitStackTraceInFastThrow
-Djdk.lang.Process.allowAmbiguousCommands=True
-Dfml.ignoreInvalidMinecraftCertificates=True
-Dfml.ignorePatchDiscrepancies=True
```

这四条**不是隐藏注入**：它们就是「设置 → 游戏全局设置 → 高级设置 → JVM 参数」输入框
的初始内容（`kaleid_api::DEFAULT_JVM_ARGS` 同时是 `GlobalSettings::jvm_args` 的默认值）。
用户可以改其中任何一条、也可以全删掉；删掉之后命令行里就真的没有，启动器不会再偷偷加回来。
旁边的「恢复默认」按钮通过 `jvm_default_args` 命令把这四条填回去——清单只存在 Rust 一侧，
界面不另抄一份，改了参数就不会出现"恢复成旧那套"。

老版本的 `settings.json` 里存的是 `jvmArgs: []`（那时默认参数由启动器暗中注入），
第一次读盘时会补上这四条并写回，同时置上 `jvmArgsInitialized`；此后空数组的含义是
"用户自己删光了"，不会再被填回去。

- `-XX:-OmitStackTraceInFastThrow`：JIT 反复抛同一个异常后会省略堆栈，崩溃报告里
  就只剩一句 `NullPointerException` 没有行号。关掉它，崩溃日志才有指向。
- `-Djdk.lang.Process.allowAmbiguousCommands=True`：允许带空格的路径以"含糊但能跑"的
  方式传给子进程。模组（尤其是旧版 Forge 生态）自建 `ProcessBuilder` 时经常不转义路径，
  没有这条就会在"用户名带空格 / 装在 `Program Files`"的机器上失败。
- `-Dfml.ignoreInvalidMinecraftCertificates=True` /
  `-Dfml.ignorePatchDiscrepancies=True`：Forge 的签名与补丁一致性检查开关。用第三方
  启动器时这些检查会误报（`Invalid signature` / `Patch discrepancy`），Forge 官方文档
  里也把这两条列为排查手段。

## 2. GC 档位

| 档位 | 参数 | 出处 |
|---|---|---|
| 尽量使用 ZGC | `-XX:+UseZGC`（Java 15+） | ZGC 本身的官方选项；系统太旧或 Java 太低时自动退回标准 G1GC |
| 尽量使用分代 ZGC | `-XX:+UseZGC -XX:+ZGenerational`（Java 21 / 22） | JEP 439。Java 23 起分代是默认模式，所以 23+ 只加 `-XX:+UseZGC`。PCL2 的默认档是这一档 |
| **标准 G1GC（默认）** | `-XX:+UseG1GC -XX:+UnlockExperimentalVMOptions -XX:G1NewSizePercent=20 -XX:G1ReservePercent=20 -XX:MaxGCPauseMillis=50`（+ 版本兼容补丁） | **Mojang** 官方启动器的 G1 参数（PCL2 里这一档直接叫 `Mojang G1GC`） |
| 调优 G1GC | 标准 G1GC + `-XX:G1HeapRegionSize=32M -XX:+PerfDisableSharedMem`，并按 Java 版本追加调优项 | **PCL2**（Plain Craft Launcher 2） |
| 不指定 | 不加任何 GC 参数 | 完全交给 JVM 的默认值 |

**默认是标准 G1GC**，不是 ZGC：ZGC 要 Java 15+（分代要 21+），而玩家机器上 Java 8 / 17
很常见，那边选了 ZGC 只会被降级、白白多一条"已降级"的说明。标准 G1GC 在任何 JDK 上都能跑，
且参数与官方启动器一致，行为可预期。

> **`-XX:+UnlockExperimentalVMOptions` 不能省。** `G1NewSizePercent` 在 HotSpot 里是
> experimental 选项（Java 8 / 17 / 21 / 25 实测都这样），不加解锁开关 JVM 会直接拒绝启动：
>
> ```text
> Error: VM option 'G1NewSizePercent' is experimental and must be enabled via -XX:+UnlockExperimentalVMOptions.
> Error: The unlock option must precede 'G1NewSizePercent'.
> Error: Could not create the Java Virtual Machine.
> ```
>
> 这是 v0.1 里真实踩到的 bug（`jvm.rs` 的 `G1_BASE` 最初漏了这一条）。现在解锁开关排在
> G1 参数第二位，并有两条测试盯着：`experimental_flags_are_unlocked_before_use`（顺序）
> 和真机用例 `generated_args_are_accepted_by_real_java`（把这台机器上每个 Java 都拿生成
> 出来的参数跑一遍 `-version`）。

### 调优 G1GC 的来源与版本补丁

主体取自 PCL2 的 `LaunchAdvanceGC` 分支：

- 仓库：<https://github.com/Meloong-Git/PCL>
- 文件：`Plain Craft Launcher 2/Modules/Minecraft/ModLaunch.vb`
  <https://github.com/Meloong-Git/PCL/blob/main/Plain%20Craft%20Launcher%202/Modules/Minecraft/ModLaunch.vb>

在 PCL2 的取值之上，Kaleid 按**目标 Java 的主版本**补几条（PCL2 里是同一个分支按版本
条件拼接的同类逻辑）：

| 条件 | 追加 | 适用范围 | 原因 |
|---|---|---|---|
| Java ≤ 7 | `-XX:MaxPermSize=512m` | 两档 G1 | 8 之前没有 Metaspace，PermGen 用这条限 |
| Java ≥ 8 | `-XX:+ParallelRefProcEnabled` | 两档 G1 | 引用处理并行，减少 GC 停顿 |
| Java ≥ 12 且内存 ≥ 4096 MB | `-XX:MinHeapFreeRatio=25` `-XX:MaxHeapFreeRatio=40` | 仅调优 G1GC | 收窄堆伸缩区间，避免 G1 反复扩缩堆 |
| Java ≥ 24 | `-XX:+UseCompactObjectHeaders` | 仅调优 G1GC | 对象头压缩（JEP 519），省内存 |

> 前两条是**兼容性**补丁，标准档也加；后两条属于"调优"，只给调优档。
> 标准 G1GC 的 GC 本体参数严格等于 Mojang 那一组（`UseG1GC` + 三个百分比），
> 不会出现"选了标准档结果和官方启动器行为不一致"。这些补丁在 ZGC 档与 `none` 档
> 都不加（`g1_version_tweaks` 只对两个 G1 档生效）。

## 3. 降级规则

选 ZGC 但条件不够时，启动器不会硬塞参数（Java 遇到不认识的 `-XX` 会**直接起不来**），
而是降级并在启动状态的说明里写清原因：

| 情况 | 结果 |
|---|---|
| Java < 15 | 退回标准 G1GC |
| Java 15–20 选「分代 ZGC」 | 退回非分代 ZGC |
| Java ≥ 21 选「分代 ZGC」 | 21/22 加 `-XX:+ZGenerational`，23+ 不加（已是默认） |
| Windows build < 17763（1809） | 退回标准 G1GC |

## 4. 生成顺序（后者覆盖前者）

```text
-Xmx / -Xms / -Djava.library.path / brand / version
  → GC 档位（含版本补丁）
  → 全局 JVM 参数（= 设置里那一栏，默认就是那四条）
  → 本版本 JVM 参数（append 追加 / override 覆盖）
```

用户只要自己写了任何一个 `-XX:+Use*GC`，整套 GC 档位就整体让位，并在说明里点名这件事——
"我明明设了 SerialGC，怎么还有 UseG1GC"这种冲突不该由用户去猜。

注意 `override`（覆盖全局）连全局栏里那四条默认参数也一起替换掉：它们在设置里，
就属于"全局那一层"。版本卡片上会写明这一点（"含默认的崩溃栈 / Forge 兼容那几条"）。
