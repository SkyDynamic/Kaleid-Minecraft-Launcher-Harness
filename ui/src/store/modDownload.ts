import { create } from 'zustand';
import { getBridge } from '../bridge';
import type { ModSearchResult, ModType } from '../bridge/types';
import { useUI } from './ui';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** 类型表是静态数据，一次启动只取一次。 */
let typesPromise: Promise<ModType[]> | null = null;

/**
 * 一页多少个模组。必须和后端 `mods.rs` 的 `RANKING_PAGE_SIZE` 保持一致：
 * 界面靠它算页数，两边不相等的话「最后一页」会算错。
 */
export const RANKING_PAGE_SIZE = 50;

interface ModDownloadState {
  open: boolean;
  openDialog: () => void;
  closeDialog: () => void;

  types: ModType[] | null;
  loadTypes: () => Promise<ModType[]>;

  /**
   * 下载量排行榜（= PCL2 的空名称空条件搜索）。
   *
   * 需求是「本次启动只获取一次」：所以缓存在 store 里而不是 Dialog 里，
   * 关掉再打开不会重新请求；只有点「刷新」才会带 `force` 绕过服务端缓存。
   * 翻页不缓存到界面（后端有 10 分钟缓存，翻回去很快），只记住当前页码。
   */
  ranking: ModSearchResult | null;
  rankingPage: number;
  rankingLoading: boolean;
  rankingError: string | null;
  /**
   * 取某一页排行。
   *
   * - `force` 跳过服务端缓存（「刷新」按钮）；
   * - `page` 0 起；不传 = 当前页；
   * - `typeId` 是类型表里的 id（`null`/空 = 全部类型）。
   */
  loadRanking: (force?: boolean, page?: number, typeId?: string | null) => Promise<void>;

  /**
   * 丢掉与「Mod 列表注释」相关的缓存。
   *
   * 后端缓存按键位区分了原文/译文，但界面这一层还留着上一档的结果；设置里
   * 切换原文/翻译后必须清掉，否则要用户手动点「刷新」才看得到变化。
   */
  invalidateForDescription: () => void;
}

export const useModDownload = create<ModDownloadState>()((set, get) => ({
  open: false,
  openDialog: () => set({ open: true }),
  closeDialog: () => set({ open: false }),

  types: null,
  loadTypes: async () => {
    const cached = get().types;
    if (cached) return cached;
    typesPromise ??= getBridge()
      .invoke<ModType[]>('mods_types')
      .then((types) => {
        set({ types });
        return types;
      })
      .catch((e) => {
        // 失败要允许下次重试，否则类型下拉会永远空着。
        typesPromise = null;
        throw e;
      });
    return typesPromise;
  },

  invalidateForDescription: () =>
    set({ ranking: null, rankingLoading: false, rankingError: null }),

  ranking: null,
  rankingPage: 0,
  rankingLoading: false,
  rankingError: null,
  loadRanking: async (force = false, page, typeId = null) => {
    const state = get();
    const target = page ?? state.rankingPage;
    if (state.rankingLoading) return;
    // 同一页已经取到了就别再打一次接口（翻回来/重开对话框都会走这里）。
    if (state.ranking && !force && target === state.rankingPage) return;
    set({ rankingLoading: true, rankingError: null, rankingPage: target });
    try {
      // **不传 rootId / versionId**：初始列表是"没有筛选条件"的下载量排行。
      // 以前按选中版本收窄分面，版本目录一叫"整合包名"整页就空了。
      const ranking = await getBridge().invoke<ModSearchResult>('mods_ranking', {
        force,
        page: target,
        typeId: typeId || null,
      });
      set({ ranking, rankingLoading: false });
    } catch (e) {
      set({ rankingLoading: false, rankingError: errText(e) });
      if (force) useUI.getState().showToast(errText(e));
    }
  },
}));
