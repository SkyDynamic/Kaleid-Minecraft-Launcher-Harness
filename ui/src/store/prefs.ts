import { create } from 'zustand';
import { getBridge } from '../bridge';

/**
 * 界面偏好（APP 级，不属于游戏设置）。
 *
 * 这些是**这个程序自己的**开关：动效强弱、主题、启动时侧栏是否收起。
 * 它们不进 `settings.json`——那份文件属于游戏侧（宿主解析、参与启动参数），
 * 混进来只会让"为什么启动器本身的行为变了"变成查不出来的问题。
 * 所以存在 localStorage，并且只在界面层生效。
 */

export type MotionPref = 'system' | 'on' | 'off';
export type ThemePref = 'system' | 'dark' | 'light';
export type ResolvedTheme = 'dark' | 'light';

export type FontPref =
  | { mode: 'system' }
  | { mode: 'family'; family: string }
  | {
      mode: 'imported';
      family: string;
      fileName: string;
      extension: string;
      dataBase64: string;
    };

const STORAGE_KEY = 'kaleid.prefs.v1';

/**
 * 界面字号以 px 表示，`13` 正好等于 theme.css 里 `body { font-size: 13px }`，
 * 也就是「100%」。改它不是改字号本身，而是按比例缩放整个界面（见 `applyFontSize`）。
 */
export const FONT_SIZE_MIN = 11;
export const FONT_SIZE_MAX = 20;
export const FONT_SIZE_DEFAULT = 13;

export interface Prefs {
  motion: MotionPref;
  theme: ThemePref;
  railCollapsedOnStart: boolean;
  /** 左右栏宽度（px）；中间区始终占剩余空间。 */
  railWidth: number;
  dockWidth: number;
  font: FontPref;
  /** 界面字号（px，等比缩放整个界面，13 = 100%）。 */
  fontSizePx: number;
  /**
   * 设置页「高级设置」卡片是否展开。
   *
   * 存进偏好而不是组件 state：这块内容几乎只在调 JVM 时打开，一次打开之后就
   * 该一直是打开的——切个页面回来又要重开一次，是很烦的小摩擦。
   */
  advancedOpen: boolean;
}

const DEFAULT_RAIL_WIDTH = 272;
const DEFAULT_DOCK_WIDTH = 240;

const DEFAULTS: Prefs = {
  motion: 'system',
  theme: 'system',
  railCollapsedOnStart: false,
  railWidth: DEFAULT_RAIL_WIDTH,
  dockWidth: DEFAULT_DOCK_WIDTH,
  font: { mode: 'system' },
  fontSizePx: FONT_SIZE_DEFAULT,
  advancedOpen: false,
};

function isMotion(value: unknown): value is MotionPref {
  return value === 'system' || value === 'on' || value === 'off';
}

function isTheme(value: unknown): value is ThemePref {
  return value === 'system' || value === 'dark' || value === 'light';
}

function normalizeWidth(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value)
    ? Math.min(720, Math.max(200, Math.round(value)))
    : fallback;
}

/** 字号只接受整数 px，越界夹到范围内；认不出来就用默认值。 */
function normalizeFontSize(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) return FONT_SIZE_DEFAULT;
  return Math.min(FONT_SIZE_MAX, Math.max(FONT_SIZE_MIN, Math.round(value)));
}

function normalizeFont(value: unknown): FontPref {
  if (!value || typeof value !== 'object') return DEFAULTS.font;
  const font = value as Partial<FontPref> & { family?: unknown };
  if (font.mode === 'system') return { mode: 'system' };
  if (font.mode === 'family' && typeof font.family === 'string' && font.family.trim()) {
    return { mode: 'family', family: font.family };
  }
  if (
    font.mode === 'imported' &&
    typeof font.family === 'string' &&
    typeof (font as { dataBase64?: unknown }).dataBase64 === 'string' &&
    typeof (font as { fileName?: unknown }).fileName === 'string' &&
    typeof (font as { extension?: unknown }).extension === 'string'
  ) {
    return {
      mode: 'imported',
      family: font.family,
      fileName: (font as { fileName: string }).fileName,
      extension: (font as { extension: string }).extension,
      dataBase64: (font as { dataBase64: string }).dataBase64,
    };
  }
  return DEFAULTS.font;
}

/** 读偏好。存储损坏或字段不认识一律回落默认值，不让一条脏数据锁住界面。 */
export function readPrefs(): Prefs {
  if (typeof localStorage === 'undefined') return { ...DEFAULTS };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<Prefs> | null;
    if (!parsed || typeof parsed !== 'object') return { ...DEFAULTS };
    return {
      motion: isMotion(parsed.motion) ? parsed.motion : DEFAULTS.motion,
      theme: isTheme(parsed.theme) ? parsed.theme : DEFAULTS.theme,
      railCollapsedOnStart:
        typeof parsed.railCollapsedOnStart === 'boolean'
          ? parsed.railCollapsedOnStart
          : DEFAULTS.railCollapsedOnStart,
      railWidth: normalizeWidth(parsed.railWidth, DEFAULT_RAIL_WIDTH),
      dockWidth: normalizeWidth(parsed.dockWidth, DEFAULT_DOCK_WIDTH),
      font: normalizeFont(parsed.font),
      fontSizePx: normalizeFontSize(parsed.fontSizePx),
      advancedOpen:
        typeof parsed.advancedOpen === 'boolean'
          ? parsed.advancedOpen
          : DEFAULTS.advancedOpen,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

function persist(prefs: Prefs) {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // 存不进去（配额/隐私模式）就只影响下次启动，不改当前行为。
  }
}

/**
 * 把动效开关写到 <html data-motion>。
 *
 * 只有明确选「关闭」时才写 `off`；「跟随系统」不写属性，交给
 * theme.css 里的 `prefers-reduced-motion` 媒体查询判断。
 */
export function applyMotion(prefs: Prefs) {
  if (typeof document === 'undefined') return;
  if (prefs.motion === 'off') {
    document.documentElement.dataset.motion = 'off';
  } else {
    delete document.documentElement.dataset.motion;
  }
}

/**
 * 「跟随系统」在**运行时**解析成 dark / light。
 *
 * 这样 theme.css 里只需要一套浅色变量，不必再写一遍 `prefers-color-scheme`
 * 媒体查询去跟属性选择器保持同步——两套浅色值迟早会漂移。
 */
export function resolveTheme(pref: ThemePref): ResolvedTheme {
  if (pref !== 'system') return pref;
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return 'dark';
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

/**
 * 写主题属性，并顺带把外壳的标题栏一起换掉。
 *
 * 只改 CSS 变量的话，浅色界面顶着一条黑色标题栏，看起来像没做完。
 * `setTheme` 在浏览器里不存在（mock 也没有），所以是可选调用。
 */
export function applyTheme(theme: ResolvedTheme) {
  if (typeof document !== 'undefined') {
    document.documentElement.dataset.theme = theme;
  }
  try {
    void getBridge().setTheme?.(theme);
  } catch {
    // 外壳不支持就算了：界面颜色已经换好了。
  }
}

/**
 * 应用界面字体。
 *
 * 默认跟随系统：移除内联变量，让 theme.css 的 `--font-sans` 生效。
 * 指定字体族或导入字体时，覆盖 `--font-sans`，保留系统字体作为 fallback。
 * 导入字体会在运行时注册为 FontFace；字体文件损坏时静默回退，不阻塞界面。
 */
export async function applyFont(prefs: Prefs): Promise<void> {
  if (typeof document === 'undefined') return;
  const root = document.documentElement;
  const font = prefs.font;
  if (font.mode === 'system') {
    root.style.removeProperty('--font-sans');
    return;
  }
  let family = font.family;
  if (font.mode === 'imported') {
    if (typeof FontFace === 'undefined' || !document.fonts) {
      root.style.removeProperty('--font-sans');
      return;
    }
    const mime =
      font.extension === 'otf'
        ? 'font/otf'
        : font.extension === 'ttc'
          ? 'font/collection'
          : 'font/ttf';
    const face = new FontFace(
      family,
      `url(data:${mime};base64,${font.dataBase64})`,
    );
    try {
      await face.load();
      document.fonts.add(face);
    } catch {
      root.style.removeProperty('--font-sans');
      return;
    }
  } else {
    family = font.family;
  }
  const quoted = `"${family.replace(/"/g, '')}"`;
  root.style.setProperty(
    '--font-sans',
    `${quoted}, "Segoe UI Variable Text", "Segoe UI", system-ui, "Microsoft YaHei UI", sans-serif`,
  );
}

/**
 * 应用界面字号。
 *
 * 做法是**整页等比缩放**：写一个 `--ui-zoom` 比例，由 theme.css 的
 * `html { zoom: var(--ui-zoom, 1) }` 生效。这样文字、间距、控件会一起变大变小，
 * 观感和浏览器缩放一致——只放大字号会让 12px 的行高、24px 的按钮和 13px 的
 * 文字互相打架，反而更难读。数据基准是 13px = 100%（和 `body` 的字号对齐），
 * 所以老用户的界面在默认值下与改动前完全一致。
 *
 * 缩放必须是**无过渡**的：字体大小跟动画一起变化看着像界面在抖。
 */
export function applyFontSize(prefs: Prefs) {
  if (typeof document === 'undefined') return;
  const root = document.documentElement;
  const scale = prefs.fontSizePx / FONT_SIZE_DEFAULT;
  root.dataset.fontSize = String(prefs.fontSizePx);
  if (Math.abs(scale - 1) < 0.001) {
    // 100% 时移除变量，退回 `zoom: 1`，避免留下一个无意义的乘数。
    root.style.removeProperty('--ui-zoom');
  } else {
    root.style.setProperty('--ui-zoom', scale.toFixed(4));
  }
}

/**
 * 当前的整页缩放比例（13px = 1）。`position: fixed` 的浮层要用它换算坐标。
 *
 * 在 `zoom` 子树里写 `left: 300` 会被再乘一遍缩放才落到屏幕上，而
 * `getBoundingClientRect()` 返回的已经是缩放后的视口坐标——两者混用就会偏：
 * 界面字号调到 16px 时菜单偏出约四分之一屏，20px 时直接跑到窗口外面。
 */
export function uiScale(): number {
  if (typeof document === 'undefined') return 1;
  const raw = document.documentElement.style.getPropertyValue('--ui-zoom');
  const value = Number.parseFloat(raw);
  return Number.isFinite(value) && value > 0 ? value : 1;
}

interface PrefsState extends Prefs {
  setMotion: (motion: MotionPref) => void;
  setTheme: (theme: ThemePref) => void;
  setRailCollapsedOnStart: (value: boolean) => void;
  setRailWidth: (width: number) => void;
  setDockWidth: (width: number) => void;
  setFont: (font: FontPref) => void;
  setFontSizePx: (px: number) => void;
  setAdvancedOpen: (open: boolean) => void;
}

const snapshot = (state: Prefs): Prefs => ({
  motion: state.motion,
  theme: state.theme,
  railCollapsedOnStart: state.railCollapsedOnStart,
  railWidth: state.railWidth,
  dockWidth: state.dockWidth,
  font: state.font,
  fontSizePx: state.fontSizePx,
  advancedOpen: state.advancedOpen,
});

export const usePrefs = create<PrefsState>()((set, get) => ({
  ...readPrefs(),

  setMotion: (motion) => {
    set({ motion });
    persist(snapshot({ ...get(), motion }));
    applyMotion({ ...get() });
  },

  setTheme: (theme) => {
    set({ theme });
    persist(snapshot({ ...get(), theme }));
    applyTheme(resolveTheme(theme));
  },

  setRailCollapsedOnStart: (railCollapsedOnStart) => {
    set({ railCollapsedOnStart });
    persist(snapshot({ ...get(), railCollapsedOnStart }));
  },

  setRailWidth: (railWidth) => {
    set({ railWidth });
    persist(snapshot({ ...get(), railWidth }));
  },

  setDockWidth: (dockWidth) => {
    set({ dockWidth });
    persist(snapshot({ ...get(), dockWidth }));
  },

  setFont: (font) => {
    set({ font });
    persist(snapshot({ ...get(), font }));
    void applyFont({ ...get(), font });
  },

  setFontSizePx: (px) => {
    const fontSizePx = normalizeFontSize(px);
    if (fontSizePx === get().fontSizePx) return;
    set({ fontSizePx });
    persist(snapshot({ ...get(), fontSizePx }));
    applyFontSize({ ...get(), fontSizePx });
  },

  setAdvancedOpen: (advancedOpen) => {
    set({ advancedOpen });
    persist(snapshot({ ...get(), advancedOpen }));
  },
}));