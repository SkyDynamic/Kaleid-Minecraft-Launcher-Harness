import type { ReactNode } from 'react';
import { Minus, Plus } from 'lucide-react';
import { IconButton, Row, SegmentedControl, Switch, TextLink } from '../../primitives';
import {
  FONT_SIZE_DEFAULT,
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  usePrefs,
  type MotionPref,
  type ThemePref,
} from '../../store/prefs';
import { useApp } from '../../store/app';
import { FontPicker } from './FontPicker';

const MOTION_OPTIONS: Array<{ value: MotionPref; label: string }> = [
  { value: 'system', label: '跟随系统' },
  { value: 'on', label: '始终开启' },
  { value: 'off', label: '关闭' },
];

const THEME_OPTIONS: Array<{ value: ThemePref; label: string }> = [
  { value: 'system', label: '跟随系统' },
  { value: 'dark', label: '深色' },
  { value: 'light', label: '浅色' },
];

const APP_VERSION = '0.1.0';
const APP_LICENSE = 'Apache License 2.0';
/** 作者与版权持有人是同一个人，所以版权行不再重复写一遍 "作者 xxx"。 */
const APP_COPYRIGHT = '© 2026 Sky_Dynamic';

/**
 * 关于块：**不是**一条设置行。
 *
 * 它没有"值"可调，套进 Row 的左右分栏只会让一长串信息挤在右半边的窄框里。
 * 所以这里改成上下结构：标题一行，下面一条一项（版本 / 协议 / 版权 / 开源许可），
 * 左列固定宽度当标签列，读起来是清单而不是段落。
 */
function AboutBlock({ pluginCount }: { pluginCount: number }) {
  const rows: Array<{ label: string; value: ReactNode }> = [
    {
      label: '软件版本',
      value: (
        <span className="font-mono">
          {APP_VERSION}
          <span className="ml-2 font-sans text-ink4">内置插件 {pluginCount} 个</span>
        </span>
      ),
    },
    {
      label: '开源协议',
      value: (
        <>
          {APP_LICENSE}
          <span className="ml-2 text-ink4">全文见随附的 LICENSE</span>
        </>
      ),
    },
    {
      label: '版权信息',
      value: (
        <>
          {APP_COPYRIGHT}
          <span className="ml-2 text-ink4">保留所有权利</span>
        </>
      ),
    },
    {
      label: '开源许可',
      value: (
        <span className="block">
          Rust · Tauri 2 · React 19 · Tailwind CSS 4 · zustand · lucide-react，分别遵循
          MIT / Apache-2.0 / ISC 许可；各依赖的许可文本随其自身分发。
        </span>
      ),
    },
  ];

  return (
    <div className="border-b border-line px-4 py-4 last:border-b-0">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <span className="text-[12.5px] font-medium text-ink">关于</span>
        {/* 标题只报产品与形态：具体版本号在下面的「软件版本」一行，不重复写两遍 */}
        <span className="text-[11px] text-ink4">Kaleid · 界面骨架 · Tauri 2 + React 19</span>
      </div>

      <dl className="m-0 mt-3 space-y-2 text-[11.5px] leading-[17px]">
        {rows.map((row) => (
          <div key={row.label} className="flex gap-3">
            <dt className="w-[64px] shrink-0 text-ink4">{row.label}</dt>
            <dd className="m-0 min-w-0 flex-1 text-ink2">{row.value}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

/**
 * 界面字号步进器。
 *
 * 只有这一个地方用得上，所以不抽成公共控件。中间显示当前 px，两侧 -/+ 一步 1px。
 * 到上下界时按钮禁用——禁用态本身就说清了"已经到头"，不需要弹提示。
 */
function FontSizeStepper() {
  const fontSizePx = usePrefs((s) => s.fontSizePx);
  const setFontSizePx = usePrefs((s) => s.setFontSizePx);

  return (
    <div className="flex items-center gap-2.5">
      {fontSizePx === FONT_SIZE_DEFAULT ? (
        <span className="text-[10.5px] text-ink4">默认 {FONT_SIZE_DEFAULT} px</span>
      ) : (
        <TextLink onClick={() => setFontSizePx(FONT_SIZE_DEFAULT)}>恢复默认</TextLink>
      )}
      <div
        role="group"
        aria-label="界面字号"
        className="inline-flex items-center gap-0.5 rounded-[8px] border border-line bg-sunken p-[3px]"
      >
        <IconButton
          label="减小字号"
          size={22}
          disabled={fontSizePx <= FONT_SIZE_MIN}
          onClick={() => setFontSizePx(fontSizePx - 1)}
        >
          <Minus size={12} />
        </IconButton>
        {/* aria-live：点一次就读一次新值，键盘/读屏用户不必再去比较界面变化 */}
        <span
          aria-live="polite"
          className="w-[52px] text-center font-mono text-[12px] tabular-nums text-ink"
        >
          {fontSizePx} px
        </span>
        <IconButton
          label="增大字号"
          size={22}
          disabled={fontSizePx >= FONT_SIZE_MAX}
          onClick={() => setFontSizePx(fontSizePx + 1)}
        >
          <Plus size={12} />
        </IconButton>
      </div>
    </div>
  );
}

/**
 * APP 设置：这个程序自己的行为。
 *
 * 与「游戏全局设置」分开放，是因为两者的归属完全不同：这里的东西只影响界面，
 * 不参与启动参数；那边的每个值都会被宿主解析进命令行。
 */
export function AppSection() {
  const motion = usePrefs((s) => s.motion);
  const setMotion = usePrefs((s) => s.setMotion);
  const theme = usePrefs((s) => s.theme);
  const setTheme = usePrefs((s) => s.setTheme);
  const railCollapsedOnStart = usePrefs((s) => s.railCollapsedOnStart);
  const setRailCollapsedOnStart = usePrefs((s) => s.setRailCollapsedOnStart);

  const pluginCount = useApp((s) => s.plugins.length);

  return (
    <div>
      <Row
        label="界面动效"
        hint="「跟随系统」尊重系统的「减少动态效果」；关闭后所有过渡立即完成，功能不受影响。"
      >
        <SegmentedControl
          ariaLabel="界面动效"
          value={motion}
          options={MOTION_OPTIONS}
          onChange={setMotion}
        />
      </Row>

      <Row label="主题" hint="跟随系统时按 Windows 的亮/暗模式切换；换主题会连带换掉窗口标题栏。">
        <SegmentedControl ariaLabel="主题" value={theme} options={THEME_OPTIONS} onChange={setTheme} />
      </Row>

      <Row
        label="界面字体"
        hint="默认跟随系统字体；可搜索系统已安装字体，或导入 ttf / otf / ttc 文件。"
      >
        <FontPicker />
      </Row>

      <Row
        label="界面字号"
        hint={`按 ${FONT_SIZE_DEFAULT} px = 100% 等比缩放整个界面：文字、间距和控件一起变大变小；修改立即生效并记住，不影响游戏。`}
      >
        <FontSizeStepper />
      </Row>

      <Row label="启动时收起侧栏" hint="只在下一次启动生效；当前是否展开仍由侧栏上的按钮决定。">
        <div className="flex items-center gap-2.5">
          {/* 文字在前、开关在后：开关是这一行的"句号"，右对齐时贴边的是它 */}
          <span className="text-[11.5px] text-ink4">
            {railCollapsedOnStart ? '启动时显示 56px 图标栏' : '启动时展开完整侧栏'}
          </span>
          <Switch
            ariaLabel="启动时收起侧栏"
            checked={railCollapsedOnStart}
            onChange={setRailCollapsedOnStart}
          />
        </div>
      </Row>

      <Row label="界面语言" hint="本地化设置">
        <SegmentedControl
          ariaLabel="界面语言"
          value="zh-CN"
          options={[{ value: 'zh-CN', label: '简体中文' }]}
          onChange={() => {}}
        />
      </Row>

      <AboutBlock pluginCount={pluginCount} />
    </div>
  );
}
