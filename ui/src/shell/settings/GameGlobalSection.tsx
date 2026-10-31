import { useEffect, useRef, useState } from 'react';
import { ChevronRight } from 'lucide-react';
import { Dialog } from '../../components/Dialog';
import { JavaPicker } from '../../components/JavaPicker';
import {
  Button,
  Collapse,
  Input,
  Row,
  SegmentedControl,
  Select,
  Slider,
  Switch,
  Textarea,
  TextLink,
} from '../../primitives';
import type { GcMode, GlobalSettings, LaunchAfterAction } from '../../bridge/types';
import { getBridge } from '../../bridge/bridge';
import { usePluginEnabled } from '../../store/app';
import { useModDownload } from '../../store/modDownload';
import { useSettings } from '../../store/settings';
import { usePrefs } from '../../store/prefs';
import { cx, formatGb } from '../../lib/util';

const HIGH_CONCURRENCY_WARNING =
  '如果设置过多的下载线程，可能会导致下载时出现非常严重的卡顿。建议设置到最大64线程即可满足下载需求，除非你知道你自己在做什么。';

const SOURCE_OPTIONS: Array<{ value: string; label: string }> = [
  { value: 'official-first', label: '官方优先' },
  { value: 'mirror-first', label: '镜像优先' },
  { value: 'official-only', label: '仅官方' },
  { value: 'mirror-only', label: '仅镜像' },
];

const MOD_SOURCE_OPTIONS: Array<{ value: string; label: string }> = [
  { value: 'official-first', label: '官方优先' },
  { value: 'mirror-first', label: '镜像优先' },
  { value: 'official-only', label: '仅官方' },
];

const MOD_DESCRIPTION_OPTIONS: Array<{ value: string; label: string }> = [
  { value: 'original', label: '原文' },
  { value: 'translated', label: '翻译' },
];

const AFTER_LAUNCH_OPTIONS: Array<{ value: LaunchAfterAction; label: string }> = [
  { value: 'keep', label: '保持不变' },
  { value: 'minimize', label: '最小化' },
  { value: 'hide_to_tray', label: '隐藏至托盘' },
  { value: 'exit', label: '退出启动器' },
];

const AFTER_LAUNCH_HINTS: Record<string, string> = {
  keep: '什么也不做，启动器留在原处。',
  minimize: '游戏起来后把启动器最小化到任务栏。',
  hide_to_tray: '游戏起来后隐藏到托盘；游戏退出后自动重新打开。',
  exit: '游戏起来后关闭启动器（游戏继续运行）。',
};

/** 文本域里的参数按行拆、去空白、丢空行，再拼回去——和落盘时的规则一致。 */
function normalizeArgs(text: string): string {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .join('\n');
}

/** GC 档位。顺序与 PCL2 的「游戏设置 → 启动 → 高级 → GC」一致。 */
const GC_OPTIONS: Array<{ value: GcMode; label: string }> = [
  { value: 'zgc', label: '尽量使用 ZGC' },
  { value: 'zgc_gen', label: '尽量使用分代 ZGC' },
  { value: 'g1', label: '标准 G1GC' },
  { value: 'g1_tuned', label: '调优 G1GC' },
  { value: 'none', label: '不指定（可自定义）' },
];

const GC_HINTS: Record<GcMode, string> = {
  zgc: 'Java 15+ 才支持；系统太旧或 Java 太低时会自动退回标准 G1GC。',
  zgc_gen:
    'Java 21+ 才支持分代 ZGC；不够时先退非分代 ZGC，再退标准 G1GC。PCL2 的默认档是这一档，但对老 Java 不友好。',
  g1: '默认档。Mojang 官方启动器用的那一组 G1 参数（-XX:+UseG1GC / G1NewSizePercent=20 / G1ReservePercent=20 / MaxGCPauseMillis=50），任何 JDK 都能跑。',
  g1_tuned:
    '在标准 G1GC 基础上加 -XX:G1HeapRegionSize=32M 与 -XX:+PerfDisableSharedMem，再按 Java 版本补堆比例 / 紧凑对象头。取值来自 PCL2（Meloong-Git/PCL，ModLaunch.vb 的 LaunchAdvanceGC）。',
  none: '启动器不加任何 GC 参数，只保留你写在下面的自定义参数。',
};

/**
 * 游戏**全局**设置：所有文件夹与版本的默认值。
 *
 * 这里是"兜底那一层"，不是版本设置页的替代品——版本级覆盖仍然在主区的
 * 「游戏设置」里，优先级 `版本 > 文件夹 > 全局 > 内置默认`。
 * 每个值都直接落到 `settings_patch_global`，宿主会校验范围并写盘。
 */
export function GameGlobalSection() {
  const global = useSettings((s) => s.global);
  const patchGlobal = useSettings((s) => s.patchGlobal);
  const [warningOpen, setWarningOpen] = useState(false);
  const warned = useRef(false);
  const [brandText, setBrandText] = useState<string | null>(null);
  // 这两组设置项属于可选插件：插件关掉后它们没有作用对象，留着只会让人以为
  // 调了有用。其余几行（内存 / Java / 启动前检查 / 隔离）是内核自己的，始终显示。
  const versionManagerEnabled = usePluginEnabled('version-manager');
  const modDownloadEnabled = usePluginEnabled('mod-download');
  // 「高级设置」默认收起：JVM 参数不是每天都改的东西，摊在首页只会挡住常用项。
  // 展开状态存在偏好里，切页面回来不会又缩回去。
  const advancedOpen = usePrefs((s) => s.advancedOpen);
  const setAdvancedOpen = usePrefs((s) => s.setAdvancedOpen);
  const [jvmText, setJvmText] = useState<string | null>(null);
  const jvmApplied = (global?.jvmArgs ?? []).join('\n');
  const jvmDirty = jvmText !== null && normalizeArgs(jvmText) !== jvmApplied;
  // 「恢复默认」要填回去的那四条，由 Rust 侧给（`jvm_default_args`）。
  // 界面不自己抄一份，免得改了参数之后按钮还在恢复旧的那套。
  const [defaultArgs, setDefaultArgs] = useState<string[] | null>(null);
  const defaultsApplied =
    defaultArgs !== null &&
    defaultArgs.length === (global?.jvmArgs.length ?? 0) &&
    defaultArgs.every((arg, index) => arg === global?.jvmArgs[index]);
  // 「使用高性能显卡」只在 Windows 上有意义：平台由宿主告知，不在界面里猜 UA
  const [gpuSupported, setGpuSupported] = useState(true);

  useEffect(() => {
    let alive = true;
    void getBridge()
      .invoke<string[]>('jvm_default_args')
      .then((list) => {
        if (alive) setDefaultArgs(list);
      })
      .catch(() => {
        // 老外壳没有这条命令也不该让整页报错：按钮就不出现
      });
    void getBridge()
      .invoke<{ os: string; highPerformanceGpuSupported: boolean }>('app_platform')
      .then((info) => {
        if (alive) setGpuSupported(info.highPerformanceGpuSupported);
      })
      .catch(() => {
        // 拿不到就按"支持"处理：Windows 是主平台，禁用开关的代价更大
      });
    return () => {
      alive = false;
    };
  }, []);

  // 输入抖动 500ms 后再落盘（和版本设置页同一套节奏）
  useEffect(() => {
    if (!jvmDirty || jvmText === null) return;
    const timer = setTimeout(() => {
      const args = jvmText
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean);
      void patchGlobal({ jvmArgs: args });
    }, 500);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jvmText, jvmDirty]);

  if (!global) {
    return <p className="px-4 py-6 text-[12px] text-ink4">正在读取全局设置…</p>;
  }

  const patch = (value: Partial<GlobalSettings>) => void patchGlobal(value);

  // 输入框用本地草稿：每敲一个字就写盘会让 settings.json 疯狂抖动，
  // 但「失焦即提交」又要保证草稿与真值不会长期分叉，所以默认取真值。
  const brandDraft = brandText ?? global.launchBrand;
  const commitBrand = () => {
    const next = brandDraft.replace(/[\r\n\t]/g, ' ').slice(0, 48);
    setBrandText(null);
    if (next !== global.launchBrand) patch({ launchBrand: next });
  };

  return (
    <div>
      <Row label="内存分配" hint="版本或文件夹没有单独指定时，启动用它。">
        <div className="flex w-full items-center justify-end gap-3">
          <Slider
            ariaLabel="全局内存分配"
            min={512}
            max={16384}
            step={512}
            value={global.memoryMb}
            onChange={(value) => patch({ memoryMb: value })}
            className="min-w-0 max-w-[320px] flex-1"
          />
          <span className="w-[54px] shrink-0 text-right font-mono text-[12px] text-ink">
            {formatGb(global.memoryMb)}
          </span>
          {global.memoryMb === 4096 ? (
            <span className="shrink-0 text-[10.5px] text-ink4">内置默认 4 GB</span>
          ) : (
            <TextLink className="shrink-0" onClick={() => patch({ memoryMb: 4096 })}>
              恢复默认
            </TextLink>
          )}
        </div>
      </Row>

      <Row label="Java 运行时" hint="版本或文件夹没有单独指定时，启动从这里挑。">
        <JavaPicker
          major={null}
          value={global.javaPath}
          onChange={(path) => patch({ javaPath: path })}
          scope="全局"
        />
      </Row>

      <Row label="启动前检查">
        <div className="flex items-center gap-2.5">
          <span className="text-[11.5px] text-ink4">
            {global.checkBeforeLaunch ? '启动前校验文件完整性' : '跳过校验，直接启动'}
          </span>
          <Switch
            ariaLabel="全局启动前检查"
            checked={global.checkBeforeLaunch}
            onChange={(checked) => patch({ checkBeforeLaunch: checked })}
          />
        </div>
      </Row>

      <Row
        label="游戏内标识"
        hint="游戏标题界面左下角版本行上的那个括号内容（Minecraft 1.20.1 (…)）。关掉或留空即不显示；版本级设置可以再覆盖。"
      >
        <div className="flex w-full flex-col items-end gap-2">
          <div className="flex w-full items-center justify-end gap-2.5">
            <Input
              aria-label="默认游戏内标识"
              value={brandDraft}
              maxLength={48}
              disabled={!global.launchBrandEnabled}
              placeholder="Kaleid MC Launcher"
              onChange={(e) => setBrandText(e.target.value)}
              onBlur={commitBrand}
              onKeyDown={(e) => {
                if (e.key === 'Enter') e.currentTarget.blur();
              }}
              className="min-w-0 max-w-[300px] flex-1"
            />
            <Switch
              ariaLabel="显示游戏内标识"
              checked={global.launchBrandEnabled}
              onChange={(checked) => patch({ launchBrandEnabled: checked })}
            />
          </div>
          <span className="text-[10.5px] text-ink4">
            {global.launchBrandEnabled
              ? brandDraft.trim()
                ? `当前：Minecraft · ${brandDraft.trim()}`
                : '文本为空 = 不显示标识'
              : '已关闭：启动参数里不带 --versionType'}
          </span>
        </div>
      </Row>

      <Row
        label="启动游戏后操作"
        hint="只影响启动器自己；游戏进程始终独立运行，关掉启动器不会结束游戏。"
      >
        <div className="flex flex-col items-end gap-2">
          <SegmentedControl
            ariaLabel="启动游戏后操作"
            value={global.launchAfterAction}
            options={AFTER_LAUNCH_OPTIONS}
            onChange={(value) => patch({ launchAfterAction: value as LaunchAfterAction })}
          />
          <span className="max-w-[320px] text-right text-[10.5px] leading-[15px] text-ink4">
            {AFTER_LAUNCH_HINTS[global.launchAfterAction]}
          </span>
        </div>
      </Row>

      <Row
        label="默认版本隔离"
        hint="文件夹选「继承全局」时才会用到它；文件夹上的显式设置永远优先。"
      >
        <div className="flex items-center gap-2.5">
          <span className="text-[11.5px] text-ink4">
            {global.isolationDefault
              ? '新版本默认使用独立目录 versions/<id>'
              : '新版本默认共用主目录'}
          </span>
          <Switch
            ariaLabel="默认版本隔离"
            checked={global.isolationDefault}
            onChange={(checked) => patch({ isolationDefault: checked })}
          />
        </div>
      </Row>

      {/* 「下载并发」是版本安装和模组下载共用的线程池，任一插件在用就得留着 */}
      {versionManagerEnabled || modDownloadEnabled ? (
        <Row
          label="下载并发"
          hint="同时下载的连接数，范围 1–256；大文件会按它切段并行取，建议不超过 64。"
        >
          <div className="flex w-full items-center justify-end gap-3">
            <Slider
              ariaLabel="下载并发"
              min={1}
              max={256}
              step={1}
              value={global.downloadConcurrency}
              onChange={(value) => {
                if (value >= 64 && global.downloadConcurrency < 64 && !warned.current) {
                  warned.current = true;
                  setWarningOpen(true);
                }
                if (value < 64) warned.current = false;
                patch({ downloadConcurrency: value });
              }}
              className="min-w-0 max-w-[320px] flex-1"
            />
            <span className="w-[42px] shrink-0 text-right font-mono text-[12px] text-ink">
              {global.downloadConcurrency}
            </span>
          </div>
        </Row>
      ) : null}

      {/* 下载源只影响游戏文件（Mojang / BMCLAPI），归「版本管理」插件 */}
      {versionManagerEnabled ? (
        <Row
          label="下载源"
          hint={
            global.downloadSource === 'mirror-first'
              ? '镜像优先：先走 BMCLAPI，失败再回官方源。'
              : global.downloadSource === 'mirror-only'
                ? '仅镜像：只走 BMCLAPI，官方源不会被访问。'
                : global.downloadSource === 'official-only'
                  ? '仅官方：只走官方源，不会自动切换到镜像。'
                  : '官方优先：先走 Mojang，连续两个 3 秒窗口低于 512 KB/s 自动切 BMCLAPI。'
          }
        >
          <SegmentedControl
            ariaLabel="下载源"
            value={global.downloadSource}
            options={SOURCE_OPTIONS}
            onChange={(value) => patch({ downloadSource: value })}
          />
        </Row>
      ) : null}

      {/* Mod 相关：属于「模组下载」插件 */}
      {modDownloadEnabled ? (
        <>
          <Row
            label="Mod 数据源"
            hint={
              global.modSource === 'mirror-first'
                ? '元数据与文件下载都按这里走。镜像优先：先用 MCIM 镜像（mod.mcimirror.top），失败回官方；CurseForge 只有镜像。'
                : global.modSource === 'official-only'
                  ? '元数据与文件下载都按这里走。仅官方：Modrinth 直连；CurseForge 官方接口要 API Key，因此仍走 MCIM 镜像。'
                  : '元数据与文件下载都按这里走。官方优先：Modrinth 先直连、失败再镜像；CurseForge 走 MCIM 镜像。'
            }
          >
            <SegmentedControl
              ariaLabel="Mod 数据源"
              value={global.modSource}
              options={MOD_SOURCE_OPTIONS}
              onChange={(value) => patch({ modSource: value })}
            />
          </Row>

          <Row
            label="Mod 列表注释"
            hint={
              global.modDescription === 'translated'
                ? '显示 MCIM 译文。不论 Mod 数据源选了什么，简介都会经 MCIM 翻译接口取译文；条目多时首次加载会慢一点。'
                : '显示模组站上的原始简介。想看图中文说明可以切成「翻译」——译名不受这一项影响。'
            }
          >
            <SegmentedControl
              ariaLabel="Mod 列表注释"
              value={global.modDescription}
              options={MOD_DESCRIPTION_OPTIONS}
              onChange={(value) => {
                patch({ modDescription: value });
                // 表单这一层也缓存了上一档的结果，切换后要让模组对话框重新取。
                useModDownload.getState().invalidateForDescription();
              }}
            />
          </Row>
        </>
      ) : null}

      {/* ---- 高级设置（可折叠）----
          JVM / GC / 显卡 / 自动中文都放这里：它们会直接影响游戏进程，
          和上面的"界面偏好"不是一类东西，混在一起容易误改。 */}
      <div className="border-t border-line">
        <button
          type="button"
          onClick={() => setAdvancedOpen(!advancedOpen)}
          aria-expanded={advancedOpen}
          className="flex w-full items-center gap-2 px-4 py-3 text-left transition-colors hover:bg-hover"
        >
          <ChevronRight
            size={12}
            className={cx(
              'shrink-0 text-ink4 transition-transform duration-150',
              advancedOpen && 'rotate-90',
            )}
          />
          <span className="text-[12.5px] font-medium text-ink">高级设置</span>
          <span className="truncate text-[10.5px] text-ink4">
            JVM 参数 · GC · 高性能显卡 · 自动中文 · LatestLaunch.bat
          </span>
        </button>
        <Collapse open={advancedOpen} unmountOnClose={false}>
          <div>
            <Row
              label="JVM 参数"
              hint={
                '一行一条，直接拼到 Java 命令行里（排在启动器 GC 档位之后，所以能覆盖它们）。\n' +
                '框里默认就是推荐的那 4 条，可以改、可以删；删光了就真的不传。点「恢复默认」随时能填回来。'
              }
            >
              <div className="flex w-full flex-col gap-1.5">
                <Textarea
                  aria-label="全局 JVM 参数"
                  rows={4}
                  value={jvmText ?? jvmApplied}
                  placeholder="-XX:+UseG1GC&#10;-Dfile.encoding=UTF-8"
                  spellCheck={false}
                  onChange={(event) => setJvmText(event.target.value)}
                  className="w-full"
                />
                <div className="flex items-center justify-between gap-2 text-[10.5px] text-ink4">
                  <span>
                    {jvmDirty ? '输入后自动保存…' : '已保存'}
                    {global.jvmArgs.length > 0 ? ` · ${global.jvmArgs.length} 条` : ' · 未设置'}
                  </span>
                  <div className="flex shrink-0 items-center gap-3">
                    {defaultArgs ? (
                      <TextLink
                        className={cx(defaultsApplied && 'pointer-events-none opacity-40')}
                        onClick={() => {
                          setJvmText(defaultArgs.join('\n'));
                          void patchGlobal({ jvmArgs: defaultArgs });
                        }}
                      >
                        恢复默认
                      </TextLink>
                    ) : null}
                    {global.jvmArgs.length > 0 ? (
                      <TextLink
                        onClick={() => {
                          setJvmText('');
                          void patchGlobal({ jvmArgs: [] });
                        }}
                      >
                        清空
                      </TextLink>
                    ) : null}
                  </div>
                </div>
              </div>
            </Row>

            <Row
              label="内存管理"
              hint={GC_HINTS[global.gcMode] ?? GC_HINTS.zgc_gen}
            >
              <Select
                ariaLabel="内存管理（GC 档位）"
                value={global.gcMode}
                options={GC_OPTIONS}
                onChange={(value) => patch({ gcMode: value as GcMode })}
                className="w-[180px]"
              />
            </Row>

            <Row
              label="使用高性能显卡"
              hint={
                gpuSupported
                  ? '把启动器和 Java 都登记成「高性能」：HKCU\\Software\\Microsoft\\DirectX\\UserGpuPreferences 下写 GpuPreference=2。写不进去只是警告，不会拦住启动。'
                  : '这项只在 Windows 上有意义（写的是 Windows 的显卡首选项注册表）；当前系统已自动禁用。'
              }
            >
              <div className="flex items-center gap-2.5">
                <span className="text-[11.5px] text-ink4">
                  {!gpuSupported
                    ? '当前系统不支持'
                    : global.highPerformanceGpu
                      ? '双显卡机器上优先用独显'
                      : '跟随系统决定'}
                </span>
                <Switch
                  ariaLabel="使用高性能显卡"
                  checked={gpuSupported && global.highPerformanceGpu}
                  disabled={!gpuSupported}
                  onChange={(checked) => patch({ highPerformanceGpu: checked })}
                />
              </div>
            </Row>

            <Row
              label="自动将游戏调为中文"
              hint="启动前把 options.txt 的 lang: 行改成 zh_cn（1.10 及更早写 zh_CN），装了 YOSBR 时改 config/yosbr/options.txt。只动这一行，且保留原来的换行风格；其它设置一字不改。"
            >
              <div className="flex items-center gap-2.5">
                <span className="text-[11.5px] text-ink4">
                  {global.autoChinese ? '启动前改成简体中文' : '不改游戏语言'}
                </span>
                <Switch
                  ariaLabel="自动将游戏调为中文"
                  checked={global.autoChinese}
                  onChange={(checked) => patch({ autoChinese: checked })}
                />
              </div>
            </Row>

            <Row
              label="保存 LatestLaunch.bat"
              hint="每次启动都把这次的真实命令写进 <数据目录>/LatestLaunch.bat（访问令牌抹成 ***）。和 PCL2 的做法一样，用来对照「启动器到底传了什么」。"
            >
              <div className="flex items-center gap-2.5">
                <span className="text-[11.5px] text-ink4">
                  {global.writeLatestLaunchBat ? '每次启动都覆盖写入' : '不写'}
                </span>
                <Switch
                  ariaLabel="保存 LatestLaunch.bat"
                  checked={global.writeLatestLaunchBat}
                  onChange={(checked) => patch({ writeLatestLaunchBat: checked })}
                />
              </div>
            </Row>
          </div>
        </Collapse>
      </div>

      <Dialog
        open={warningOpen}
        onClose={() => setWarningOpen(false)}
        title="高并发下载提醒"
        width={460}
        footer={
          <div className="flex justify-end">
            <Button variant="primary" onClick={() => setWarningOpen(false)}>
              知道了
            </Button>
          </div>
        }
      >
        <p className="p-4 text-[12px] leading-[20px] text-ink2">{HIGH_CONCURRENCY_WARNING}</p>
      </Dialog>
    </div>
  );
}
