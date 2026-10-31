import { create } from 'zustand';
import { getBridge } from '../bridge';
import type {
  Account,
  IsolationMode,
  PluginInfo,
  Root,
  VersionSummary,
} from '../bridge/types';
import { useSettings } from './settings';
import { useUI } from './ui';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));
const PREFERRED_VERSION = '1.21.4-fabric';

interface AppState {
  roots: Root[];
  activeRootId: string | null;
  /** 默认目录（`roots_list` 已把它排在第一）。 */
  defaultRootId: string | null;
  /** 需要弹「选择游戏目录」时置 true；单目录时不弹。 */
  rootChooserOpen: boolean;
  versions: VersionSummary[];
  activeVersionId: string | null;
  accounts: Account[];
  activeAccount: Account | null;
  plugins: PluginInfo[];

  ready: boolean;
  scanning: boolean;

  init: () => Promise<void>;
  selectRoot: (id: string) => Promise<void>;
  selectVersion: (id: string) => Promise<void>;
  addRoot: (path: string, alias?: string) => Promise<Root>;
  removeRoot: (id: string) => Promise<void>;
  renameRoot: (id: string, alias: string) => Promise<void>;
  setDefaultRoot: (id: string | null) => Promise<void>;
  setIsolation: (mode: IsolationMode) => Promise<void>;
  setRootIsolation: (id: string, mode: IsolationMode) => Promise<void>;
  refreshVersions: () => Promise<void>;
  refreshAccounts: () => Promise<void>;
  setActiveAccount: (id: string) => Promise<void>;
  applyOffline: (name: string) => Promise<Account>;
  removeAccount: (id: string) => Promise<void>;
  togglePlugin: (id: string, enabled: boolean) => Promise<void>;
}

let bootstrap: Promise<void> | null = null;

export const useApp = create<AppState>()((set, get) => ({
  roots: [],
  activeRootId: null,
  defaultRootId: null,
  rootChooserOpen: false,
  versions: [],
  activeVersionId: null,
  accounts: [],
  activeAccount: null,
  plugins: [],

  ready: false,
  scanning: false,

  init: () => {
    if (bootstrap) return bootstrap;
    bootstrap = (async () => {
      const bridge = getBridge();
      try {
        const [roots, plugins, accounts, active, defaultRoot] = await Promise.all([
          bridge.invoke<Root[]>('roots_list'),
          bridge.invoke<PluginInfo[]>('plugins_list'),
          bridge.invoke<Account[]>('accounts_list'),
          bridge.invoke<Account | null>('accounts_active'),
          bridge.invoke<Root | null>('roots_default'),
        ]);
        set({ roots, plugins, accounts, activeAccount: active, defaultRootId: defaultRoot?.id ?? null });
        await useSettings.getState().loadGlobal();
        // 默认目录优先；没设过默认才按老规矩回落 Roaming / 第一个。
        //
        // 这里**不再弹「这次用哪个目录」**：有多个目录时也直接用默认的那个。
        // 想换目录走左栏目录菜单的「切换游戏目录…」，选择器还在，只是不再挡启动。
        const root = defaultRoot ?? roots.find((r) => r.id === 'roaming') ?? roots[0] ?? null;
        if (root) {
          await get().selectRoot(root.id);
        } else {
          set({ ready: true });
        }
      } catch (e) {
        useUI.getState().showToast(errText(e));
        set({ ready: true });
      }
    })();
    return bootstrap;
  },

  selectRoot: async (id) => {
    const bridge = getBridge();
    set({ scanning: true });
    try {
      const root = await bridge.invoke<Root>('roots_select', { id });
      const [roots, versions] = await Promise.all([
        bridge.invoke<Root[]>('roots_list'),
        bridge.invoke<VersionSummary[]>('versions_scan', { rootId: root.id }),
      ]);
      const keep = get().activeVersionId;
      const version =
        versions.find((v) => v.id === keep) ??
        versions.find((v) => v.id === PREFERRED_VERSION) ??
        versions[0] ??
        null;
      set({ roots, activeRootId: root.id, versions, activeVersionId: version?.id ?? null });
      if (version) {
        await useSettings.getState().loadVersion(root.id, version.id);
      } else {
        useSettings.setState({ version: null, effective: null });
      }
    } catch (e) {
      useUI.getState().showToast(errText(e));
    } finally {
      set({ scanning: false, ready: true });
    }
  },

  selectVersion: async (id) => {
    const { activeRootId } = get();
    set({ activeVersionId: id });
    if (activeRootId) await useSettings.getState().loadVersion(activeRootId, id);
  },

  addRoot: async (path, alias) => {
    const bridge = getBridge();
    const root = await bridge.invoke<Root>('roots_add', { path, alias });
    await get().selectRoot(root.id);
    return root;
  },

  removeRoot: async (id) => {
    const bridge = getBridge();
    try {
      await bridge.invoke<null>('roots_remove', { id });
      const roots = await bridge.invoke<Root[]>('roots_list');
      set({ roots, defaultRootId: get().defaultRootId === id ? null : get().defaultRootId });
      if (get().activeRootId === id) {
        const next = roots[0] ?? null;
        if (next) {
          await get().selectRoot(next.id);
        } else {
          set({ activeRootId: null, versions: [], activeVersionId: null });
          useSettings.setState({ version: null, effective: null });
        }
      }
      useUI.getState().showToast('已从列表移除');
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  renameRoot: async (id, alias) => {
    try {
      await getBridge().invoke<Root>('roots_rename', { id, alias });
      const roots = await getBridge().invoke<Root[]>('roots_list');
      set({ roots });
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  setDefaultRoot: async (id) => {
    try {
      const bridge = getBridge();
      if (id === null) {
        await bridge.invoke<null>('roots_clear_default');
      } else {
        await bridge.invoke<Root>('roots_set_default', { id });
      }
      const roots = await bridge.invoke<Root[]>('roots_list');
      set({ roots, defaultRootId: id });
      useUI.getState().showToast(id === null ? '已取消默认目录' : '已设为默认目录');
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  setIsolation: async (mode) => {
    const { activeRootId } = get();
    if (!activeRootId) return;
    await get().setRootIsolation(activeRootId, mode);
  },

  setRootIsolation: async (id, mode) => {
    const { activeVersionId, roots } = get();
    try {
      const root = await getBridge().invoke<Root>('roots_set_isolation', { id, mode });
      set({ roots: roots.map((r) => (r.id === root.id ? root : r)) });
      if (activeVersionId && id === get().activeRootId) {
        await useSettings.getState().refreshEffective(id, activeVersionId);
      }
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  refreshVersions: async () => {
    const { activeRootId } = get();
    if (!activeRootId) return;
    set({ scanning: true });
    try {
      const versions = await getBridge().invoke<VersionSummary[]>('versions_scan', {
        rootId: activeRootId,
      });
      set({ versions });
    } catch (e) {
      useUI.getState().showToast(errText(e));
    } finally {
      set({ scanning: false });
    }
  },

  refreshAccounts: async () => {
    const bridge = getBridge();
    try {
      const [accounts, active] = await Promise.all([
        bridge.invoke<Account[]>('accounts_list'),
        bridge.invoke<Account | null>('accounts_active'),
      ]);
      set({ accounts, activeAccount: active });
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  setActiveAccount: async (id) => {
    try {
      const active = await getBridge().invoke<Account>('accounts_set_active', { id });
      set({ activeAccount: active });
      await get().refreshAccounts();
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  applyOffline: async (name) => {
    const account = await getBridge().invoke<Account>('accounts_apply_offline', { name });
    set({ activeAccount: account });
    await get().refreshAccounts();
    return account;
  },

  removeAccount: async (id) => {
    try {
      await getBridge().invoke<null>('accounts_remove', { id });
      await get().refreshAccounts();
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  togglePlugin: async (id, enabled) => {
    try {
      const plugins = await getBridge().invoke<PluginInfo[]>('plugins_set_enabled', {
        id,
        enabled,
      });
      set({ plugins });
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },
}));

/* -------- selectors -------- */

export function useActiveRoot(): Root | null {
  return useApp((s) => s.roots.find((r) => r.id === s.activeRootId) ?? null);
}

export function useActiveVersion(): VersionSummary | null {
  return useApp((s) => s.versions.find((v) => v.id === s.activeVersionId) ?? null);
}

export function usePluginEnabled(id: string): boolean {
  return useApp((s) => s.plugins.find((p) => p.id === id)?.enabled === true);
}
