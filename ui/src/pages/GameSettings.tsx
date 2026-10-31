import { useEffect, useState } from 'react';
import { ChevronRight } from 'lucide-react';
import type { GcMode, IsolationMode, JvmArgsMode } from '../bridge/types';
import { openDirInShell } from '../bridge';
import {
  Card,
  Collapse,
  Input,
  Row,
  SegmentedControl,
  Select,
  Slider,
  Switch,
  Textarea,
  TextLink,
} from '../primitives';
import { JavaPicker } from '../components/JavaPicker';
import { useApp } from '../store/app';
import { useSettings } from '../store/settings';
import { usePrefs } from '../store/prefs';
import { useUI } from '../store/ui';
import { cx, formatGb } from '../lib/util';

const JVM_MODE_OPTIONS: Array<{ value: JvmArgsMode; label: string }> = [
  { value: 'append', label: '追加到全局' },
  { value: 'override', label: '覆盖全局' },
];

/** 文本域里的参数按行拆、去空白、丢空行，再拼回去——和落盘时的规则一致。 */
function normalizeArgs(text: string): string {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .join('\n');
}

/** 版本级 GC：多一个「跟随全局」。 */
const GC_OPTIONS_WITH_INHERIT: Array<{ value: string; label: string }> = [
  { value: 'inherit', label: '跟随全局' },
  { value: 'zgc', label: '尽量使用 ZGC' },
  { value: 'zgc_gen', label: '尽量使用分代 ZGC' },
  { value: 'g1', label: '标准 G1GC' },
  { value: 'g1_tuned', label: '调优 G1GC' },
  { value: 'none', label: '不指定（可自定义）' },
];

const GC_HINT_TEXT: Record<string, string> = {
  inherit: '用全局的 GC 档位；Java 版本不够时启动器会自动降级并在启动状态里说明。',
  zgc: 'Java 15+ 才支持 ZGC，不够时退回标准 G1GC。',
  zgc_gen: 'Java 21+ 才有分代 ZGC，不够时退非分代 ZGC、再退标准 G1GC。',
  g1: '默认档。Mojang 官方启动器用的标准 G1GC 参数，任何 JDK 都能跑。',
  g1_tuned: '在标准 G1GC 上加 G1HeapRegionSize=32M 与 PerfDisableSharedMem，取值来自 PCL2。',
  none: '不加任何 GC 参数，只保留你写的自定义参数。',
};

const ISOLATION_OPTIONS: Array<{ value: IsolationMode; label: string }> = [
  { value: 'inherit', label: '继承全局' },
  { value: 'on', label: '开启' },
  { value: 'off', label: '关闭' },
];

const WINDOW_OPTIONS = [
  { value: '854x480', label: '854×480' },
  { value: '1280x720', label: '1280×720' },
  { value: 'fullscreen', label: '全屏' },
];

const WINDOW_LABELS: Record<string, string> = {
  '854x480': '854×480',
  '1280x720': '1280×720',
  fullscreen: '全屏',
};

export function GameSettings() {
  const root = useApp((s) => s.roots.find((r) => r.id === s.activeRootId) ?? null);
  const version = useApp((s) => s.versions.find((v) => v.id === s.activeVersionId) ?? null);
  const setIsolation = useApp((s) => s.setIsolation);
  const vset = useSettings((s) => s.version);
  const effective = useSettings((s) => s.effective);
  const global = useSettings((s) => s.global);
  const patchVersion = useSettings((s) => s.patchVersion);
  const highlight = useUI((s) => s.highlightRow);
  const highlightNonce = useUI((s) => s.highlightNonce);
  const clearHighlight = useUI((s) => s.clearHighlight);
  const showToast = useUI((s) => s.showToast);

  const [jvmText, setJvmText] = useState('');
  const [brandText, setBrandText] = useState('');

  const versionId = version?.id ?? null;
  const jvmApplied = (vset?.jvmArgs ?? []).join('\n');
  // 比较"归一化后"的文本：粘贴进来的多余空行不该让界面一直显示"待保存"
  const jvmDirty = normalizeArgs(jvmText) !== jvmApplied;
  const brandApplied = vset?.launchBrand ?? '';
  const brandDirty = brandText !== brandApplied;

  // Re-seed the editor only when the selected version changes, so in-flight
  // typing is never overwritten by a store refresh.
  useEffect(() => {
    setJvmText((useSettings.getState().version?.jvmArgs ?? []).join('\n'));
    setBrandText(useSettings.getState().version?.launchBrand ?? '');
  }, [versionId]);

  // Live commit (debounced) — there is no save button anywhere in the app.
  useEffect(() => {
    if (!jvmDirty || !root || !version) return;
    const timer = setTimeout(() => {
      const args = jvmText
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean);
      void patchVersion(root.id, version.id, { jvmArgs: args });
    }, 500);    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jvmText, jvmDirty, root?.id, version?.id]);

  useEffect(() => {
    if (!highlight) return;
    const el = document.getElementById(`setting-${highlight}`);
    el?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    const timer = setTimeout(() => clearHighlight(), 1700);
    return () => clearTimeout(timer);
  }, [highlight, highlightNonce, clearHighlight]);

  /**
   * 真的把两个目录在资源管理器里打开。
   *
   * 之前这里是一句"（演示环境）"的假提示——点了什么都不会发生。现在照实打开：
   * 主目录 + 这个版本的目录，方便对比隔离到底把数据放哪了。
   */
  /**
   * 「高级设置」折叠卡：与全局设置共用同一个展开状态（存 `usePrefs`）。
   *
   * 左栏焦点指令点名 `jvm` 时强制展开——否则用户点「JVM 参数」跳过来，
   * 看到的是一个收起的卡片，等于跳空了。
   */
  const advancedOpen = usePrefs((s) => s.advancedOpen);
  const setAdvancedOpen = usePrefs((s) => s.setAdvancedOpen);
  useEffect(() => {
    if (highlight === 'jvm') setAdvancedOpen(true);
  }, [highlight, setAdvancedOpen]);

  const openDirPair = async () => {
    if (!root || !version) return;
    const base = root.path.replace(/[\\/]+$/, '');
    const versionDir = `${base}\\versions\\${version.id}`;
    const opened: string[] = [];
    for (const dir of [base, versionDir]) {
      if (opened.includes(dir)) continue;
      if (await openDirInShell(dir)) opened.push(dir);
    }
    if (opened.length === 0) {
      showToast('打开目录失败：这个外壳不支持，或者目录不存在');
    }
  };

  if (!root || !version || !vset) {
    return (
      <div className="px-5 py-6 text-[12px] text-ink4">先在左侧选择一个版本，再调整它的设置。</div>
    );
  }

  const memoryValue = vset.memoryMb ?? effective?.memoryMb ?? global?.memoryMb ?? 4096;
  const windowValue = vset.window ?? effective?.window ?? '1280x720';
  const checkValue =
    vset.checkBeforeLaunch ?? effective?.checkBeforeLaunch ?? global?.checkBeforeLaunch ?? true;
  const brandEnabled = vset.launchBrandEnabled ?? global?.launchBrandEnabled ?? true;

  const patch = (value: Parameters<typeof patchVersion>[2]) => {
    void patchVersion(root.id, version.id, value);
  };

  const commitJvm = () => {
    const args = jvmText
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean);
    if (args.join('\n') === jvmApplied) return;
    patch({ jvmArgs: args });
  };

  const commitBrand = () => {
    if (!brandDirty) return;
    patch({ launchBrand: brandText.replace(/[\r\n\t]/g, ' ').slice(0, 48) });
  };

  return (
    <div className="p-3">
      <div className="mb-2 flex items-baseline gap-2 px-1">
        <h2 className="text-[13px] font-semibold text-ink">游戏设置</h2>
        <span className="text-[11px] text-ink4">
          设置项修改后立即生效 · 本版本未设置的项继承全局
        </span>
      </div>

      <Card className="overflow-hidden">
        <Row
          id="setting-isolation"
          label="版本隔离"
          hint="作用于本文件夹下的所有版本；切换不会迁移已有文件。"
          flashing={highlight === 'isolation'}
        >
          <div className="flex max-w-full flex-col items-end gap-2">
            <SegmentedControl
              ariaLabel="版本隔离"
              value={root.isolation}
              options={ISOLATION_OPTIONS}
              onChange={(mode) => void setIsolation(mode)}
            />
            <div className="flex flex-wrap items-baseline justify-end gap-x-2 gap-y-1 text-[11.5px] leading-[16px]">
              <span className="text-ink3">
                当前生效：
                {effective?.isolated ? '开启（版本独立目录）' : '关闭（共用主目录）'}
              </span>
              <span className="text-ink4">来源：{effective?.isolatedFrom ?? '全局默认'}</span>
            </div>
            <TextLink onClick={() => void openDirPair()}>
              打开主目录与版本目录
            </TextLink>
          </div>
        </Row>

        <Row
          id="setting-memory"
          label="内存分配"
          flashing={highlight === 'memory'}
        >
          <div className="flex w-full items-center justify-end gap-3">
            <Slider
              ariaLabel="内存分配"
              min={512}
              max={16384}
              step={512}
              value={memoryValue}
              onChange={(value) => patch({ memoryMb: value })}
              className="min-w-0 max-w-[320px] flex-1"
            />
            <span className="w-[54px] shrink-0 text-right font-mono text-[12px] text-ink">
              {formatGb(memoryValue)}
            </span>
            {vset.memoryMb !== null ? (
              <TextLink className="shrink-0" onClick={() => patch({ memoryMb: null })}>
                恢复继承
              </TextLink>
            ) : (
              <span className="shrink-0 text-[10.5px] text-ink4">
                继承全局 {formatGb(global?.memoryMb ?? 4096)}
              </span>
            )}
          </div>
        </Row>

        <Row id="setting-java" label="Java 运行时">
          <JavaPicker
            major={version.javaMajor}
            value={vset.javaPath}
            onChange={(path) => patch({ javaPath: path })}
            scope="本版本"
          />
        </Row>

        <Row
          id="setting-brand"
          label="游戏内标识"
          hint="显示在游戏标题界面左下角的版本行上（Minecraft 1.20.1 (…））。关掉后启动参数里不再带 --versionType，和原版启动完全一致。"
          flashing={highlight === 'brand'}
        >
          <div className="flex max-w-full flex-col items-end gap-2">
            <div className="flex w-full items-center justify-end gap-2.5">
              <Input
                aria-label="游戏内标识文本"
                value={brandText}
                maxLength={48}
                disabled={!brandEnabled}
                placeholder="Kaleid MC Launcher"
                onChange={(e) => setBrandText(e.target.value)}
                onBlur={commitBrand}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') e.currentTarget.blur();
                }}
                className="min-w-0 max-w-[280px] flex-1"
              />
              <Switch
                ariaLabel="显示游戏内标识"
                checked={brandEnabled}
                onChange={(checked) => patch({ launchBrandEnabled: checked })}
              />
            </div>
            <div className="flex flex-wrap items-baseline justify-end gap-x-2 gap-y-1 text-[11.5px] leading-[16px]">
              <span className="text-ink3">
                当前生效：
                {effective?.launchBrand ? `Minecraft · ${effective.launchBrand}` : '不显示标识'}
              </span>
              {vset.launchBrand !== null || vset.launchBrandEnabled !== null ? (
                <TextLink
                  className="shrink-0"
                  onClick={() => patch({ launchBrand: null, launchBrandEnabled: null })}
                >
                  恢复继承
                </TextLink>
              ) : (
                <span className="text-ink4">
                  继承全局 {global?.launchBrandEnabled === false ? '（已关闭）' : global?.launchBrand || '默认'}
                </span>
              )}
            </div>
          </div>
        </Row>

        <Row id="setting-window" label="窗口">
          <div className="flex flex-col items-end gap-2">
            <SegmentedControl
              ariaLabel="游戏窗口"
              value={windowValue}
              options={WINDOW_OPTIONS}
              onChange={(value) => patch({ window: value })}
            />
            <div className="text-[10.5px] text-ink4">
              当前生效：{WINDOW_LABELS[effective?.window ?? '1280x720'] ?? windowValue}
              {vset.window === null ? ' · 继承默认' : ''}
            </div>
          </div>
        </Row>

        <Row id="setting-check" label="启动前检查">
          <div className="flex items-center gap-2.5">
            <span className="text-[11.5px] text-ink4">
              {checkValue ? '启动前校验文件完整性' : '跳过校验，直接启动'}
            </span>
            <Switch
              ariaLabel="启动前检查"
              checked={checkValue}
              onChange={(checked) => patch({ checkBeforeLaunch: checked })}
            />
          </div>
        </Row>
      </Card>

      {/* ---- 高级设置（可折叠）----
          JVM 参数与 GC 会直接改变 Java 命令行，单独一张卡收着；
          「追加 / 覆盖」这一对选项决定它和全局参数的关系。
          用 Card 而不是自己拼圆角：卡片样式（圆角/边框/底色）只在一处定义。 */}
      <Card className="mt-3 overflow-hidden">
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
            本版本的 JVM 参数 · GC 档位（{vset.jvmArgsMode === 'override' ? '覆盖全局' : '追加到全局'}）
          </span>
        </button>
        <Collapse open={advancedOpen} unmountOnClose={false}>
          <div className="border-t border-line">
            <Row
              id="setting-jvm"
              label="参数方式"
              hint="「追加」= 全局参数在前、本版本在后；「覆盖」= 全局参数整段不进命令行。GC 档位两种模式都照旧生效。"
              flashing={highlight === 'jvm'}
            >
              <div className="flex flex-col items-end gap-2">
                <SegmentedControl
                  ariaLabel="JVM 参数方式"
                  value={vset.jvmArgsMode ?? 'append'}
                  options={JVM_MODE_OPTIONS}
                  onChange={(value) => patch({ jvmArgsMode: value as JvmArgsMode })}
                />
                <span className="max-w-[340px] text-right text-[10.5px] leading-[15px] text-ink4">
                  {vset.jvmArgsMode === 'override'
                    ? `全局那 ${global?.jvmArgs.length ?? 0} 条不会进这个版本（含默认的崩溃栈 / Forge 兼容那几条）`
                    : `全局的 ${global?.jvmArgs.length ?? 0} 条参数会排在前面`}
                </span>
              </div>
            </Row>

            <Row
              label="本版本 JVM 参数"
              hint="一行一条。留空 = 该版本只用全局参数（覆盖模式下则是完全不用全局参数）。"
            >
              <div className="flex w-full flex-col gap-1.5">
                <Textarea
                  aria-label="本版本 JVM 参数"
                  rows={4}
                  value={jvmText}
                  onChange={(e) => setJvmText(e.target.value)}
                  onBlur={commitJvm}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
                      e.currentTarget.blur();
                    }
                  }}
                  placeholder="-XX:+UseG1GC"
                  spellCheck={false}
                  className="w-full"
                />
                <div className="flex items-center justify-between gap-2 text-[10.5px] text-ink4">
                  <span>
                    {jvmDirty ? '失焦或等一下会自动保存…' : '已保存'}
                    {vset.jvmArgs.length > 0 ? ` · ${vset.jvmArgs.length} 条` : ' · 未设置'}
                  </span>
                  <span className="font-mono">
                    生效 {effective?.jvmArgs.length ?? 0} 条
                  </span>
                </div>
              </div>
            </Row>

            <Row
              label="内存管理"
              hint={GC_HINT_TEXT[vset.gcMode ?? 'inherit'] ?? GC_HINT_TEXT.inherit}
            >
              <Select
                ariaLabel="内存管理（GC 档位）"
                value={vset.gcMode ?? 'inherit'}
                options={GC_OPTIONS_WITH_INHERIT}
                onChange={(value) =>
                  patch({ gcMode: value === 'inherit' ? null : (value as GcMode) })
                }
                className="w-[180px]"
              />
            </Row>
          </div>
        </Collapse>
      </Card>
    </div>
  );
}
