import { create } from 'zustand';
import { getBridge } from '../bridge/bridge';
import type { CrashArchive, LaunchState } from '../bridge/types';

/**
 * 启动会话的**唯一**轮询入口（`launch_states`）。
 *
 * 以前 MainPane 自己轮询一份、概览页想显示实例还得再轮询一份，两边不同步时
 * 会出现"概览说没在跑、按钮说在跑"。现在都读这里。
 *
 * 只在一个会话处于 `preparing`（有进度条要动）时提速到 250 ms，
 * 其它时候 900 ms 就够。
 */
interface LaunchStoreState {
  sessions: LaunchState[];
  /** 最近一次轮询的错误（比如启动插件被停用）。 */
  error: string | null;
  /** 手动刷一次。 */
  refresh: () => Promise<LaunchState[]>;
  /** 开始/停止轮询（引用计数，多个组件同时用也不会互相打断）。 */
  watch: () => () => void;
  cancel: (sessionId: number) => Promise<void>;
  stop: (sessionId: number | null) => Promise<void>;
  /** 弹系统目录选择器，把排障包打成 zip。返回导出结果（用户取消返回 null）。 */
  exportZip: (sessionId: number) => Promise<CrashArchive | null>;
}

let watchers = 0;
let timer: number | null = null;
let inflight = false;

function isBusy(state: LaunchState): boolean {
  return (
    state.phase === 'preparing' || state.phase === 'launching' || state.phase === 'running'
  );
}

export function launchIsBusy(state: LaunchState): boolean {
  return isBusy(state);
}

/** 有实例在准备阶段 → 轮询要快一点，进度条才不卡顿。 */
function wantsFastPoll(sessions: LaunchState[]): boolean {
  return sessions.some((s) => s.phase === 'preparing');
}

export const useLaunch = create<LaunchStoreState>()((set, get) => ({
  sessions: [],
  error: null,

  refresh: async () => {
    if (inflight) return get().sessions;
    inflight = true;
    try {
      const sessions = await getBridge().invoke<LaunchState[]>('launch_states', {});
      set({ sessions, error: null });
      return sessions;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      set({ error: message });
      return get().sessions;
    } finally {
      inflight = false;
    }
  },

  watch: () => {
    watchers += 1;
    const tick = () => {
      void get().refresh();
      // 每一轮都按当前状态决定下一次的间隔：准备阶段 250 ms，其余 900 ms。
      const delay = wantsFastPoll(get().sessions) ? 250 : 900;
      timer = window.setTimeout(tick, delay);
    };
    if (timer === null) tick();
    return () => {
      watchers = Math.max(0, watchers - 1);
      if (watchers === 0 && timer !== null) {
        window.clearTimeout(timer);
        timer = null;
      }
    };
  },

  cancel: async (sessionId) => {
    await getBridge().invoke('launch_cancel', { sessionId });
    await get().refresh();
  },

  stop: async (sessionId) => {
    await getBridge().invoke('launch_stop', { sessionId: sessionId ?? undefined });
    await get().refresh();
  },

  exportZip: async (sessionId) => {
    // 目录由用户自己选：导到固定位置会让人找不到文件。
    const dir = await getBridge().pickFolder?.();
    if (!dir) return null;
    const archive = await getBridge().invoke<CrashArchive>('crash_export_zip', {
      sessionId,
      targetDir: dir,
    });
    return archive;
  },
}));
