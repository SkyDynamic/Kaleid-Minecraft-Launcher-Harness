import { create } from 'zustand';
import { getBridge } from '../bridge/bridge';
import type { DownloadSnapshot } from '../bridge/types';

interface DownloadState {
  snapshot: DownloadSnapshot | null;
  error: string | null;
  refresh: () => Promise<void>;
  cancel: (id: string) => Promise<void>;
  clearFinished: () => Promise<void>;
}

let inflight = false;

export const useDownloads = create<DownloadState>()((set) => ({
  snapshot: null,
  error: null,

  refresh: async () => {
    if (inflight) return;
    inflight = true;
    try {
      const snapshot = await getBridge().invoke<DownloadSnapshot>('downloads_snapshot');
      set({ snapshot, error: null });
    } catch (error) {
      set({ snapshot: null, error: error instanceof Error ? error.message : String(error) });
    } finally {
      inflight = false;
    }
  },

  cancel: async (id) => {
    await getBridge().invoke('downloads_cancel', { id });
    await useDownloads.getState().refresh();
  },

  clearFinished: async () => {
    await getBridge().invoke('downloads_clear_finished');
    await useDownloads.getState().refresh();
  },
}));

export function hasActiveDownloads(snapshot: DownloadSnapshot | null): boolean {
  return Boolean(
    snapshot?.jobs.some(
      (job) =>
        job.status === 'running' ||
        job.status === 'queued' ||
        job.status === 'resolving',
    ),
  );
}
