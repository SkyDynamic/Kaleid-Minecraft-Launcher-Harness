/**
 * 本地内容（模组 / 资源包 / 存档 / 日志）的唯一数据源。
 *
 * 四条规则：
 * 1. 路径只从 `content_dirs` 拿——它按「版本隔离」算好了目录，界面不拼字符串；
 * 2. 切版本 / 切文件夹 / 改隔离都会让整份缓存失效（`key` 一变就重读）；
 * 3. 任何写操作（启停、导入、删除）之后**重新读一遍**，不做本地乐观更新：
 *    磁盘才是真相，`.jar` ↔ `.jar.disabled` 的重命名由宿主负责；
 * 4. 失败一律抛给调用方，由页面决定是弹窗还是提示——尤其是删除失败
 *    （游戏正占用文件），必须让用户看到系统给的原因。
 */
import { useEffect } from 'react';
import { create } from 'zustand';
import { getBridge } from '../bridge';
import type {
  ContentDirs,
  ContentKind,
  ImportOutcome,
  LocalMod,
  LocalPack,
  LocalSave,
  LogFile,
  LogTail,
} from '../bridge/types';
import { useApp } from './app';
import { useSettings } from './settings';
import { useUI } from './ui';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

interface ContentState {
  dirs: ContentDirs | null;
  mods: LocalMod[];
  packs: LocalPack[];
  saves: LocalSave[];
  logs: LogFile[];

  /** 当前缓存对应的 `root|version|isolated`；`null` = 还没读过。 */
  key: string | null;
  loading: boolean;
  error: string | null;

  refresh: () => Promise<void>;
  setModEnabled: (file: string, enabled: boolean) => Promise<void>;
  importFiles: (kind: ContentKind, paths: string[]) => Promise<ImportOutcome>;
  /** 删除失败会抛出（`ApiError::Io` 的系统原因），调用方负责弹窗。 */
  remove: (kind: ContentKind, file: string) => Promise<void>;
  readLog: (file: string, maxBytes?: number) => Promise<LogTail>;
}

function target(): { rootId: string; versionId: string } | null {
  const { activeRootId, activeVersionId } = useApp.getState();
  if (!activeRootId || !activeVersionId) return null;
  return { rootId: activeRootId, versionId: activeVersionId };
}

export const useContent = create<ContentState>()((set, get) => ({
  dirs: null,
  mods: [],
  packs: [],
  saves: [],
  logs: [],
  key: null,
  loading: false,
  error: null,

  refresh: async () => {
    const ids = target();
    if (!ids) {
      set({
        dirs: null,
        mods: [],
        packs: [],
        saves: [],
        logs: [],
        key: null,
        error: null,
        loading: false,
      });
      return;
    }
    const bridge = getBridge();
    set({ loading: true });
    try {
      const dirs = await bridge.invoke<ContentDirs>('content_dirs', ids);
      const [mods, packs, saves, logs] = await Promise.all([
        bridge.invoke<LocalMod[]>('content_mods_list', ids),
        bridge.invoke<LocalPack[]>('content_packs_list', ids),
        bridge.invoke<LocalSave[]>('content_saves_list', ids),
        bridge.invoke<LogFile[]>('content_logs_list', ids),
      ]);
      set({
        dirs,
        mods,
        packs,
        saves,
        logs,
        key: `${ids.rootId}|${ids.versionId}|${dirs.isolated}`,
        error: null,
      });
    } catch (e) {
      // 插件被停用 / 目录读不了：这里只记录，页面自己显示提示
      set({ error: errText(e) });
    } finally {
      set({ loading: false });
    }
  },

  setModEnabled: async (file, enabled) => {
    const ids = target();
    if (!ids) return;
    await getBridge().invoke<LocalMod[]>('content_mods_set_enabled', {
      ...ids,
      file,
      enabled,
    });
    await get().refresh();
  },

  importFiles: async (kind, paths) => {
    const ids = target();
    if (!ids) throw new Error('先选择一个版本');
    const outcome = await getBridge().invoke<ImportOutcome>('content_import', {
      ...ids,
      kind,
      paths,
    });
    await get().refresh();
    return outcome;
  },

  remove: async (kind, file) => {
    const ids = target();
    if (!ids) throw new Error('先选择一个版本');
    await getBridge().invoke<null>('content_delete', { ...ids, kind, file });
    await get().refresh();
  },

  readLog: async (file, maxBytes) => {
    const ids = target();
    if (!ids) throw new Error('先选择一个版本');
    return getBridge().invoke<LogTail>('content_logs_read', {
      ...ids,
      file,
      maxBytes,
    });
  },
}));

/**
 * 跟着「文件夹 / 版本 / 隔离」自动重读。
 *
 * 每个内容页都调一次：宿主侧没有推送事件（`listen` 还是空实现），
 * 所以"什么时候失效"必须由界面自己判定。
 */
export function useContentSync(enabled = true): void {
  const rootId = useApp((s) => s.activeRootId);
  const versionId = useApp((s) => s.activeVersionId);
  const isolated = useSettings((s) => s.effective?.isolated ?? null);
  const refresh = useContent((s) => s.refresh);

  useEffect(() => {
    if (!enabled) return;
    void refresh();
  }, [enabled, rootId, versionId, isolated, refresh]);
}

/**
 * 导入一批文件并给出一句人话小结。
 *
 * 抽出来是因为模组页和资源包页的提示语完全一样，分散写迟早会不一样。
 */
export async function importInto(kind: ContentKind, paths: string[]): Promise<void> {
  const label = kind === 'mod' ? '模组' : '资源包';
  if (paths.length === 0) return;
  try {
    const outcome = await useContent.getState().importFiles(kind, paths);
    const skipped = outcome.skipped.length;
    const copied = outcome.copied.length;
    if (copied === 0 && skipped > 0) {
      const first = outcome.skipped[0];
      useUI
        .getState()
        .showToast(`没有导入任何${label}：${first.file} ${first.reason}`);
      return;
    }
    useUI
      .getState()
      .showToast(
        skipped > 0
          ? `已导入 ${copied} 个${label}，跳过 ${skipped} 个（同名不覆盖）`
          : `已导入 ${copied} 个${label}`,
      );
  } catch (e) {
    useUI.getState().showToast(errText(e));
  }
}
