import { create } from 'zustand';
import { readPrefs } from './prefs';

export const TAB_IDS = ['overview', 'mods', 'packs', 'saves', 'settings', 'logs'] as const;
export type TabId = (typeof TAB_IDS)[number];

export type SettingsRowId =
  | 'isolation'
  | 'memory'
  | 'java'
  | 'jvm'
  | 'window'
  | 'brand'
  | 'check';

export const SETTINGS_SECTIONS = ['app', 'game', 'plugins'] as const;
export type SettingsSection = (typeof SETTINGS_SECTIONS)[number];

interface UIState {
  tab: TabId;
  setTab: (tab: TabId) => void;

  railCollapsed: boolean;
  toggleRail: () => void;

  /**
   * 设置 Dialog。
   *
   * 设置**不再是主区里的一个页面**：主区是"这个版本的游戏"，设置是"这个程序与
   * 游戏全局"。所以它有自己的开合状态，而不是复用 `tab`。
   */
  settingsOpen: boolean;
  settingsSection: SettingsSection;
  openSettings: (section?: SettingsSection) => void;
  closeSettings: () => void;
  setSettingsSection: (section: SettingsSection) => void;

  highlightRow: SettingsRowId | null;
  highlightNonce: number;
  gotoSettingRow: (row: SettingsRowId) => void;
  clearHighlight: () => void;

  toast: string | null;
  showToast: (message: string) => void;

  focusVersionsNonce: number;
  focusVersions: () => void;

  addRootOpen: boolean;
  setAddRootOpen: (open: boolean) => void;

  /** 「获取新版本」向导。 */
  installOpen: boolean;
  openInstall: () => void;
  closeInstall: () => void;

  /** 下载队列详情 Dialog。右栏的队列区域任意位置都能打开它。 */
  queueOpen: boolean;
  setQueueOpen: (open: boolean) => void;
}

let toastTimer: ReturnType<typeof setTimeout> | undefined;

export const useUI = create<UIState>()((set, get) => ({
  tab: 'overview',
  setTab: (tab) => set({ tab }),

  railCollapsed: readPrefs().railCollapsedOnStart,
  toggleRail: () => set((s) => ({ railCollapsed: !s.railCollapsed })),

  settingsOpen: false,
  settingsSection: 'app',
  openSettings: (section) =>
    set((s) => ({ settingsOpen: true, settingsSection: section ?? s.settingsSection })),
  closeSettings: () => set({ settingsOpen: false }),
  setSettingsSection: (settingsSection) => set({ settingsSection }),

  highlightRow: null,
  highlightNonce: 0,
  gotoSettingRow: (row) =>
    set((s) => ({ tab: 'settings', highlightRow: row, highlightNonce: s.highlightNonce + 1 })),
  clearHighlight: () => set({ highlightRow: null }),

  toast: null,
  showToast: (message) => {
    if (get().toast === message) return;
    set({ toast: message });
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => set({ toast: null }), 2800);
  },

  focusVersionsNonce: 0,
  focusVersions: () =>
    set((s) => ({ railCollapsed: false, focusVersionsNonce: s.focusVersionsNonce + 1 })),

  addRootOpen: false,
  setAddRootOpen: (open) => set({ addRootOpen: open }),

  installOpen: false,
  openInstall: () => set({ installOpen: true }),
  closeInstall: () => set({ installOpen: false }),

  queueOpen: false,
  setQueueOpen: (queueOpen) => set({ queueOpen }),
}));
