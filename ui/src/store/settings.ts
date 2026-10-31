import { create } from 'zustand';
import { getBridge } from '../bridge';
import type { EffectiveConfig, GlobalSettings, VersionSettings } from '../bridge/types';
import { useUI } from './ui';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

interface SettingsState {
  global: GlobalSettings | null;
  version: VersionSettings | null;
  effective: EffectiveConfig | null;

  loadGlobal: () => Promise<void>;
  loadVersion: (rootId: string, versionId: string) => Promise<void>;
  refreshEffective: (rootId: string, versionId: string) => Promise<void>;
  patchGlobal: (patch: Partial<GlobalSettings>) => Promise<void>;
  patchVersion: (
    rootId: string,
    versionId: string,
    patch: Partial<VersionSettings>,
  ) => Promise<void>;
}

export const useSettings = create<SettingsState>()((set, get) => ({
  global: null,
  version: null,
  effective: null,

  loadGlobal: async () => {
    try {
      const global = await getBridge().invoke<GlobalSettings>('settings_global');
      set({ global });
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  loadVersion: async (rootId, versionId) => {
    try {
      const bridge = getBridge();
      const [version, effective] = await Promise.all([
        bridge.invoke<VersionSettings>('settings_version', { rootId, versionId }),
        bridge.invoke<EffectiveConfig>('settings_effective', { rootId, versionId }),
      ]);
      set({ version, effective });
    } catch (e) {
      set({ version: null, effective: null });
      useUI.getState().showToast(errText(e));
    }
  },

  refreshEffective: async (rootId, versionId) => {
    try {
      const effective = await getBridge().invoke<EffectiveConfig>('settings_effective', {
        rootId,
        versionId,
      });
      set({ effective });
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  patchGlobal: async (patch) => {
    try {
      const global = await getBridge().invoke<GlobalSettings>('settings_patch_global', { patch });
      set({ global });
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },

  patchVersion: async (rootId, versionId, patch) => {
    try {
      const version = await getBridge().invoke<VersionSettings>('settings_patch_version', {
        rootId,
        versionId,
        patch,
      });
      set({ version });
      await get().refreshEffective(rootId, versionId);
    } catch (e) {
      useUI.getState().showToast(errText(e));
    }
  },
}));
