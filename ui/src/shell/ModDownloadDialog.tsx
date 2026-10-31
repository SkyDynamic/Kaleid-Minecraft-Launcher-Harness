import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import {
  AlertTriangle,
  ArrowLeft,
  ArrowUp,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Download,
  ExternalLink,
  Loader2,
  RefreshCw,
  RotateCcw,
  Search,
} from 'lucide-react';
import { getBridge, openExternal } from '../bridge';
import type {
  InstallQueued,
  ModDetail,
  ModFile,
  ModHit,
  ModSearchResult,
  ModSourceFilter,
  ModTarget,
  ModVersionGroup,
} from '../bridge/types';
import { Dialog } from '../components/Dialog';
import { ModRow } from '../components/ModRow';
import { Button, Card, Collapse, Input, Select, type SelectOption } from '../primitives';
import { cx, formatBytes, formatCount, formatDate, formatRelative } from '../lib/util';
import { useApp } from '../store/app';
import { RANKING_PAGE_SIZE, useModDownload } from '../store/modDownload';
import { useUI } from '../store/ui';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** 列表一页多少个；与后端 `RANKING_PAGE_SIZE` 一致。 */
const PAGE_SIZE = RANKING_PAGE_SIZE;

/** 滚动超过这个比例才显示「回到顶部」。 */
const TOP_BUTTON_RATIO = 0.6;

const SOURCE_OPTIONS: Array<{ value: ModSourceFilter; label: string }> = [
  { value: 'all', label: '全部' },
  { value: 'modrinth', label: 'Modrinth' },
  { value: 'curseforge', label: 'CurseForge' },
];

const ALL_TYPES = '';

/** `1.21.4` → `1.21`：详情页按「加载器 + 版本家族」分组。 */
function familyOf(version: string): string {
  const parts = version.trim().split('.');
  if (parts.length < 2) return version.trim();
  const [major, minor] = parts;
  if (!/^\d+$/.test(major) || !/^\d+$/.test(minor)) return version.trim();
  return `${Number(major)}.${Number(minor)}`;
}

export function ModDownloadDialog() {
  const open = useModDownload((s) => s.open);
  const closeDialog = useModDownload((s) => s.closeDialog);
  const types = useModDownload((s) => s.types);
  const loadTypes = useModDownload((s) => s.loadTypes);
  const ranking = useModDownload((s) => s.ranking);
  const rankingLoading = useModDownload((s) => s.rankingLoading);
  const rankingError = useModDownload((s) => s.rankingError);
  const rankingPage = useModDownload((s) => s.rankingPage);
  const loadRanking = useModDownload((s) => s.loadRanking);

  const rootId = useApp((s) => s.activeRootId);
  const versionId = useApp((s) => s.activeVersionId);

  const [target, setTarget] = useState<ModTarget | null>(null);

  // 搜索卡片的状态
  const [query, setQuery] = useState('');
  const [source, setSource] = useState<ModSourceFilter>('all');
  const [typeId, setTypeId] = useState(ALL_TYPES);
  const [result, setResult] = useState<ModSearchResult | null>(null);
  const [searched, setSearched] = useState(false);
  const [searchPage, setSearchPage] = useState(0);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);

  // 「回到顶部」：Dialog 的内容区是滚动容器，得拿它的 ref 才能读滚动位置。
  const listBodyRef = useRef<HTMLDivElement | null>(null);
  const [showTopButton, setShowTopButton] = useState(false);

  // 详情（第二层视图，仍在同一个 Dialog 里）
  const [detail, setDetail] = useState<{
    title: string;
    data: ModDetail | null;
    loading: boolean;
    error: string | null;
  } | null>(null);

  // 安装确认
  const [confirm, setConfirm] = useState<{ file: ModFile; group: ModVersionGroup } | null>(null);
  const [overwrite, setOverwrite] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);

  // 打开时：类型表 + 排行榜（本次启动只取一次，force 才刷新）
  useEffect(() => {
    if (!open) return;
    setDetail(null);
    setConfirm(null);
    void loadTypes().catch((e) => useUI.getState().showToast(errText(e)));
    void loadRanking();
  }, [open, loadTypes, loadRanking]);

  // 目标版本：决定「装到哪里」、以及详情页默认展开哪一组
  useEffect(() => {
    if (!open || !rootId || !versionId) {
      setTarget(null);
      return;
    }
    let alive = true;
    getBridge()
      .invoke<ModTarget>('mods_target', { rootId, versionId })
      .then((value) => {
        if (alive) setTarget(value);
      })
      .catch(() => {
        if (alive) setTarget(null);
      });
    return () => {
      alive = false;
    };
  }, [open, rootId, versionId]);

  /**
   * 跑一次搜索。
   *
   * 空名称 + 全部来源 + 全部类型**直接走排行榜**：PCL2 的「空条件搜索」出来的
   * 就是下载量排行，两边必须是同一份列表，不然用户会以为有两个不同的空列表。
   */
  const runSearch = useCallback(
    async (force: boolean, page = 0) => {
      const empty = query.trim() === '' && source === 'all' && typeId === ALL_TYPES;
      if (empty) {
        setSearched(false);
        setSearchError(null);
        await useModDownload.getState().loadRanking(force, 0, null);
        return;
      }
      setSearching(true);
      setSearchError(null);
      try {
        const value = await getBridge().invoke<ModSearchResult>('mods_search', {
          query,
          source,
          typeId: typeId || null,
          page,
          pageSize: PAGE_SIZE,
          force,
        });
        setResult(value);
        setSearchPage(page);
        setSearched(true);
      } catch (e) {
        setSearchError(errText(e));
      } finally {
        setSearching(false);
      }
    },
    [query, source, typeId],
  );

  const resetConditions = () => {
    setQuery('');
    setSource('all');
    setTypeId(ALL_TYPES);
    setResult(null);
    setSearched(false);
    setSearchPage(0);
    setSearchError(null);
    void loadRanking(false, 0, null);
  };

  /** 「刷新」= 强制刷新元数据，不看服务端缓存；停在当前页。 */
  const refresh = () => {
    if (searched) void runSearch(true, searchPage);
    else void loadRanking(true, rankingPage, typeId);
  };

  /** 当前生效的页码（搜索/排行各记各的）。 */
  const currentPage = searched ? searchPage : rankingPage;

  const gotoPage = (next: number) => {
    const target = Math.min(Math.max(next, 0), pageCount - 1);
    if (target === currentPage) return;
    listBodyRef.current?.scrollTo({ top: 0, behavior: 'auto' });
    if (searched) void runSearch(false, target);
    else void loadRanking(false, target, typeId);
  };

  /** 类型筛选对**排行**同样生效（PCL2 的类型筛选作用于整个列表）。 */
  const changeType = (value: string) => {
    setTypeId(value);
    if (!searched) {
      listBodyRef.current?.scrollTo({ top: 0, behavior: 'auto' });
      void loadRanking(false, 0, value || null);
    }
  };

  const scrollListToTop = () => {
    listBodyRef.current?.scrollTo({ top: 0, behavior: 'smooth' });
  };

  const openDetail = async (hit: ModHit) => {
    const title = hit.translatedName?.trim() || hit.name;
    setDetail({ title, data: null, loading: true, error: null });
    try {
      const data = await getBridge().invoke<ModDetail>('mods_detail', {
        source: hit.source,
        projectId: hit.id,
        rootId,
        versionId,
      });
      setDetail({ title, data, loading: false, error: null });
    } catch (e) {
      setDetail({ title, data: null, loading: false, error: errText(e) });
    }
  };

  const reloadDetail = async () => {
    if (!detail?.data) return;
    const project = detail.data.project;
    try {
      const data = await getBridge().invoke<ModDetail>('mods_detail', {
        source: project.source,
        projectId: project.id,
        rootId,
        versionId,
        force: true,
      });
      setDetail({ title: detail.title, data, loading: false, error: null });
    } catch {
      // 刷新失败不影响「已加入队列」这个事实，静默即可。
    }
  };

  const doInstall = async () => {
    if (!confirm || !rootId || !versionId) return;
    setInstalling(true);
    try {
      await getBridge().invoke<InstallQueued>('mods_install', {
        rootId,
        versionId,
        source: confirm.file.source,
        projectId: confirm.file.projectId,
        fileId: confirm.file.fileId,
        overwrite,
      });
      useUI.getState().showToast('已加入下载队列');
      setConfirm(null);
      setOverwrite(false);
      void reloadDetail();
    } catch (e) {
      // Toast 会飘走，覆盖冲突这种「必须用户做选择」的错要留在对话框里。
      const message = errText(e);
      setInstallError(message);
      useUI.getState().showToast(message);
    } finally {
      setInstalling(false);
    }
  };

  const shown = searched ? result : ranking;
  const hits = shown?.hits ?? [];
  const warnings = shown?.warnings ?? [];
  const listLoading = searching || (!searched && rankingLoading);

  /**
   * 一共多少页。
   *
   * 排行榜只有 Modrinth 一个来源（后端明确不混源，否则会混进"这个版本装不上"
   * 的模组）；搜索时两个来源各自按同一偏移翻页，所以页数取两者里**大的**那个
   * ——较小的那个翻到底后就只是不再贡献行，不会提前结束分页。
   */
  const pageCount = searched
    ? Math.max(
        1,
        Math.ceil(
          Math.max(shown?.modrinthTotal ?? 0, shown?.curseforgeTotal ?? 0) / PAGE_SIZE,
        ),
      )
    : Math.max(1, Math.ceil((shown?.modrinthTotal ?? 0) / PAGE_SIZE));

  // 滚动监听挂在 Dialog 的内容区上：列表本身不滚动，滚动条长在内容区。
  // 换页/换视图（列表 ↔ 详情）时内容高度完全不同，所以依赖里带上它们。
  useEffect(() => {
    const el = listBodyRef.current;
    if (!open || !el) {
      setShowTopButton(false);
      return;
    }
    const handler = () => {
      const scrollable = el.scrollHeight - el.clientHeight;
      const ratio = scrollable > 0 ? el.scrollTop / scrollable : 0;
      setShowTopButton(ratio > TOP_BUTTON_RATIO);
    };
    el.addEventListener('scroll', handler, { passive: true });
    handler();
    return () => el.removeEventListener('scroll', handler);
  }, [open, detail, searched, currentPage, hits.length]);

  const typeOptions = useMemo<SelectOption[]>(() => {
    const options: SelectOption[] = [{ value: ALL_TYPES, label: '全部类型' }];
    for (const type of types ?? []) {
      options.push({ value: type.id, label: type.label, group: type.group });
    }
    return options;
  }, [types]);

  const subtitle = target
    ? `${target.versionId} · ${target.loaderLabel} ${target.gameVersion} → ${target.modsDir}`
    : '还没有选择版本';

  const title = detail ? detail.title : '下载 Mod';

  return (
    <>
      <Dialog
        open={open}
        onClose={closeDialog}
        title={title}
        subtitle={subtitle}
        width={880}
        bodyClassName="p-3"
        bodyRef={listBodyRef}
        overlay={
          showTopButton && !detail ? (
            <button
              type="button"
              onClick={scrollListToTop}
              aria-label="回到顶部"
              title="回到顶部"
              className="pointer-events-auto absolute bottom-4 right-4 grid h-8 w-8 place-items-center rounded-full border border-line-strong bg-layer2 text-ink2 shadow-[var(--shadow-overlay)] transition-colors hover:bg-hover hover:text-ink"
            >
              <ArrowUp size={14} />
            </button>
          ) : undefined
        }
      >
        {detail ? (
          <DetailView
            detail={detail}
            target={target}
            onBack={() => setDetail(null)}
            onOpenProject={openDetail}
            onPickFile={(file, group) => {
              setOverwrite(false);
              setInstallError(null);
              setConfirm({ file, group });
            }}
          />
        ) : (
          <div className="flex flex-col gap-3">
            <Card className="px-3 py-2.5">
              <div className="flex items-end gap-2">
                <label className="flex min-w-0 flex-1 flex-col gap-1">
                  <span className="text-[10.5px] text-ink4">名称</span>
                  <div className="relative">
                    <Search
                      size={11}
                      className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-ink4"
                    />
                    <Input
                      value={query}
                      onChange={(event) => setQuery(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter') void runSearch(false);
                      }}
                      placeholder="输入模组名称或 slug，留空看下载量排行"
                      className="h-[28px] pl-6 text-[11.5px]"
                      aria-label="模组名称"
                      spellCheck={false}
                    />
                  </div>
                </label>

                <div className="flex shrink-0 flex-col gap-1">
                  <span className="text-[10.5px] text-ink4">来源</span>
                  <Select
                    ariaLabel="来源"
                    value={source}
                    options={SOURCE_OPTIONS}
                    onChange={(value) => setSource(value as ModSourceFilter)}
                    className="w-[112px]"
                  />
                </div>

                <div className="flex shrink-0 flex-col gap-1">
                  <span className="text-[10.5px] text-ink4">类型</span>
                  <Select
                    ariaLabel="类型"
                    value={typeId}
                    options={typeOptions}
                    onChange={changeType}
                    className="w-[150px]"
                    listWidth={208}
                    maxHeight={320}
                  />
                </div>
              </div>

              <div className="mt-2.5 flex items-center gap-2 border-t border-line pt-2.5">
                <Button
                  variant="primary"
                  size="sm"
                  icon={searching ? <Loader2 size={11} className="animate-spin" /> : <Search size={11} />}
                  onClick={() => void runSearch(false)}
                  disabled={searching}
                >
                  搜索
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  icon={<RotateCcw size={11} />}
                  onClick={resetConditions}
                  disabled={searching}
                >
                  重置条件
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  icon={<RefreshCw size={11} />}
                  onClick={refresh}
                  disabled={searching || rankingLoading}
                  title="强制刷新元数据，不看缓存"
                >
                  刷新
                </Button>
                <span className="ml-auto font-mono text-[10px] text-ink4">
                  {searched
                    ? `Modrinth ${shown?.modrinthTotal ?? 0} · CurseForge ${shown?.curseforgeTotal ?? 0}`
                    : '按下载量排行'}
                </span>
              </div>
            </Card>

            {target?.warning ? (
              <p className="flex items-center gap-1.5 rounded-[7px] border border-line bg-layer2 px-2.5 py-1.5 text-[11px] text-ink3">
                <AlertTriangle size={11} className="shrink-0 text-amber-500" />
                {target.warning}
              </p>
            ) : null}

            {warnings.length > 0 ? (
              <div className="flex flex-col gap-1 rounded-[7px] border border-line bg-layer2 px-2.5 py-1.5">
                {warnings.map((message) => (
                  <p key={message} className="flex items-start gap-1.5 text-[11px] text-ink3">
                    <AlertTriangle size={11} className="mt-[2px] shrink-0 text-amber-500" />
                    <span className="min-w-0">{message}</span>
                  </p>
                ))}
              </div>
            ) : null}

            {searchError ? (
              <p className="rounded-[7px] border border-red/40 bg-red/10 px-2.5 py-1.5 text-[11px] text-red">
                {searchError}
              </p>
            ) : null}

            <Card className="overflow-hidden">
              {listLoading && hits.length === 0 ? (
                <p className="flex items-center gap-2 px-3 py-4 text-[11.5px] text-ink4">
                  <Loader2 size={12} className="animate-spin" />
                  {searched ? '正在搜索…' : '正在获取下载量排行…'}
                </p>
              ) : null}
              {!listLoading && hits.length === 0 ? (
                <p className="px-3 py-4 text-[11.5px] text-ink4">
                  {searched
                    ? '没有匹配的模组，换个名字或把类型换成「全部类型」'
                    : (rankingError ?? '暂时没有排行数据')}
                </p>
              ) : null}
              {hits.map((hit) => (
                <ModRow
                  key={`${hit.source}:${hit.id}`}
                  hit={hit}
                  onClick={() => void openDetail(hit)}
                />
              ))}
            </Card>

            {/* 分页器：一页 50 个，和 PCL2 一样按页翻而不是无限滚动 */}
            {!listLoading && hits.length > 0 ? (
              <div className="flex items-center justify-center gap-2 pb-1">
                <Button
                  variant="ghost"
                  size="sm"
                  icon={<ChevronLeft size={11} />}
                  disabled={currentPage <= 0}
                  onClick={() => gotoPage(currentPage - 1)}
                >
                  上一页
                </Button>
                <span className="font-mono text-[10.5px] text-ink3">
                  第 {currentPage + 1} / {pageCount} 页 · 每页 {PAGE_SIZE}
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  icon={<ChevronRight size={11} />}
                  disabled={currentPage + 1 >= pageCount}
                  onClick={() => gotoPage(currentPage + 1)}
                >
                  下一页
                </Button>
              </div>
            ) : null}
          </div>
        )}
      </Dialog>

      <Dialog
        open={confirm !== null}
        onClose={() => {
          if (!installing) setConfirm(null);
        }}
        title="确认安装"
        width={470}
        footer={
          <div className="flex items-center justify-end gap-2">
            <Button variant="secondary" onClick={() => setConfirm(null)} disabled={installing}>
              取消
            </Button>
            <Button
              variant="primary"
              icon={installing ? <Loader2 size={11} className="animate-spin" /> : <Download size={11} />}
              onClick={() => void doInstall()}
              disabled={installing || !rootId || !versionId}
            >
              安装
            </Button>
          </div>
        }
      >
        {confirm ? (
          <>
            <InstallConfirm
              file={confirm.file}
              group={confirm.group}
              target={target}
              installed={detail?.data?.project.installed ?? null}
              overwrite={overwrite}
              onOverwriteChange={setOverwrite}
              onOpenPage={(url) => void openExternal(url)}
            />
            {installError ? (
              <p className="mt-2 rounded-[7px] border border-red/40 bg-red/10 px-2.5 py-1.5 text-[11px] text-red">
                {installError}
              </p>
            ) : null}
          </>
        ) : null}
      </Dialog>
    </>
  );
}

/* ------------------------------------------------------------------ *
 * 详情
 * ------------------------------------------------------------------ */

interface DetailViewProps {
  detail: { title: string; data: ModDetail | null; loading: boolean; error: string | null };
  target: ModTarget | null;
  onBack: () => void;
  onOpenProject: (hit: ModHit) => void;
  onPickFile: (file: ModFile, group: ModVersionGroup) => void;
}

function DetailView({ detail, target, onBack, onOpenProject, onPickFile }: DetailViewProps) {
  const data = detail.data;
  const [versionFilter, setVersionFilter] = useState('');
  const [expanded, setExpanded] = useState<string[]>([]);

  const defaultKey = useMemo(() => {
    if (!data?.groups.length) return null;
    if (target) {
      const family = familyOf(target.gameVersion);
      const hit = data.groups.find(
        (group) => group.loader === target.loader && group.gameVersion === family,
      );
      if (hit) return `${hit.loader}:${hit.gameVersion}`;
    }
    const installed = data.project.installed;
    if (installed) {
      const withInstalled = data.groups.find((group) =>
        group.files.some((file) => file.fileName === installed.fileName),
      );
      if (withInstalled) return `${withInstalled.loader}:${withInstalled.gameVersion}`;
    }
    const first = data.groups[0];
    return `${first.loader}:${first.gameVersion}`;
  }, [data, target]);

  useEffect(() => {
    setVersionFilter('');
    setExpanded(defaultKey ? [defaultKey] : []);
  }, [defaultKey, detail.title]);

  if (detail.loading) {
    return (
      <p className="flex items-center gap-2 px-1 py-6 text-[11.5px] text-ink4">
        <Loader2 size={12} className="animate-spin" />
        正在读取模组详情…
      </p>
    );
  }

  if (!data) {
    return (
      <div className="flex flex-col gap-3">
        <Button variant="ghost" size="sm" icon={<ArrowLeft size={11} />} onClick={onBack}>
          返回列表
        </Button>
        <p className="rounded-[7px] border border-red/40 bg-red/10 px-2.5 py-2 text-[11.5px] text-red">
          {detail.error ?? '没能读取这个模组'}
        </p>
      </div>
    );
  }

  const project = data.project;
  const groups = versionFilter
    ? data.groups.filter((group) => group.gameVersion === versionFilter)
    : data.groups;
  const name = project.translatedName?.trim();
  const description = project.translatedDescription?.trim() || project.description;

  const toggle = (key: string) =>
    setExpanded((prev) => (prev.includes(key) ? prev.filter((k) => k !== key) : [...prev, key]));

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" icon={<ArrowLeft size={11} />} onClick={onBack}>
          返回列表
        </Button>
        <div className="ml-auto flex items-center gap-2">
          <Button
            variant="ghost"
            size="sm"
            icon={<ExternalLink size={11} />}
            onClick={() => void openExternal(project.pageUrl)}
          >
            {project.source === 'modrinth' ? 'Modrinth' : 'CurseForge'}
          </Button>
          {project.wikiUrl ? (
            <Button
              variant="ghost"
              size="sm"
              icon={<ExternalLink size={11} />}
              onClick={() => void openExternal(project.wikiUrl as string)}
            >
              MC 百科
            </Button>
          ) : null}
        </div>
      </div>

      <Card className="px-3 py-2.5">
        <div className="flex items-start gap-3">
          <span className="flex h-[44px] w-[44px] shrink-0 items-center justify-center overflow-hidden rounded-[8px] border border-line bg-layer2">
            {project.iconUrl ? (
              <img
                src={project.iconUrl}
                alt=""
                referrerPolicy="no-referrer"
                className="h-full w-full object-cover"
              />
            ) : (
              <span className="text-[15px] font-semibold text-ink4">
                {(name ?? project.name).slice(0, 1).toUpperCase()}
              </span>
            )}
          </span>
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-baseline gap-1 text-[13.5px] leading-[19px]">
              {name ? (
                <>
                  <span className="truncate font-semibold text-ink">{name}</span>
                  <span className="shrink-0 text-ink4">|</span>
                  <span className="truncate text-ink3" title={project.name}>
                    {project.name}
                  </span>
                </>
              ) : (
                <span className="truncate font-semibold text-ink">{project.name}</span>
              )}
            </div>
            <p className="mt-1 text-[11.5px] leading-[17px] text-ink2">{description}</p>
            <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-[10.5px] text-ink4">
              <span>{project.loadersLabel}</span>
              <span>{project.gameRange}</span>
              <span>{formatCount(project.downloads)} 次下载</span>
              <span>更新于 {formatRelative(project.updatedAt)}</span>
              {project.installed ? (
                <span className="rounded-[4px] bg-accent/15 px-1.5 py-[1px] text-[10px] text-accent">
                  已安装 {project.installed.fileName}
                </span>
              ) : null}
            </div>
            {project.tags.length > 0 ? (
              <div className="mt-1.5 flex flex-wrap gap-1">
                {project.tags.slice(0, 8).map((tag) => (
                  <span
                    key={tag}
                    className="rounded-[4px] bg-hover px-1.5 py-[1px] text-[10px] text-ink3"
                  >
                    {tag}
                  </span>
                ))}
              </div>
            ) : null}
          </div>
        </div>
      </Card>

      {data.warnings.length > 0 ? (
        <div className="flex flex-col gap-1 rounded-[7px] border border-line bg-layer2 px-2.5 py-1.5">
          {data.warnings.map((message) => (
            <p key={message} className="flex items-start gap-1.5 text-[11px] text-ink3">
              <AlertTriangle size={11} className="mt-[2px] shrink-0 text-amber-500" />
              <span className="min-w-0">{message}</span>
            </p>
          ))}
        </div>
      ) : null}

      {data.versions.length > 0 ? (
        <div className="flex flex-wrap items-center gap-1">
          <FilterChip active={versionFilter === ''} onClick={() => setVersionFilter('')}>
            全部版本
          </FilterChip>
          {data.versions.map((version) => (
            <FilterChip
              key={version}
              active={versionFilter === version}
              onClick={() => setVersionFilter(version)}
            >
              {version}
            </FilterChip>
          ))}
        </div>
      ) : null}

      <Card className="overflow-hidden">
        {groups.length === 0 ? (
          <p className="px-3 py-4 text-[11.5px] text-ink4">这个筛选下没有可用的版本</p>
        ) : null}
        {groups.map((group) => {
          const key = `${group.loader}:${group.gameVersion}`;
          const isOpen = expanded.includes(key);
          const depCount = group.dependencies.length;
          return (
            <div key={key} className="border-b border-line last:border-b-0">
              <button
                type="button"
                onClick={() => toggle(key)}
                aria-expanded={isOpen}
                className="flex h-[34px] w-full items-center gap-2 px-3 text-left transition-colors hover:bg-hover"
              >
                {isOpen ? (
                  <ChevronDown size={12} className="shrink-0 text-ink4" />
                ) : (
                  <ChevronRight size={12} className="shrink-0 text-ink4" />
                )}
                <span className="text-[12px] font-medium text-ink">
                  {group.loaderLabel} {group.gameVersion}
                </span>
                <span className="text-[10.5px] text-ink4">{group.files.length} 个文件</span>
                {depCount > 0 ? (
                  <span className="rounded-[4px] bg-hover px-1.5 py-[1px] text-[10px] text-ink3">
                    {depCount} 个前置
                  </span>
                ) : null}
              </button>

              <Collapse open={isOpen}>
                <div className="border-t border-line bg-layer1/40">
                  {depCount > 0 ? (
                    <div>
                      <p className="px-3 pt-2 text-[10px] uppercase tracking-[0.08em] text-ink4">
                        前置资源
                      </p>
                      {group.dependencies.map((hit) => (
                        <ModRow
                          key={`${hit.source}:${hit.id}`}
                          hit={hit}
                          onClick={() => onOpenProject(hit)}
                        />
                      ))}
                    </div>
                  ) : null}
                  <div>
                    {depCount > 0 ? (
                      <p className="px-3 pt-2 text-[10px] uppercase tracking-[0.08em] text-ink4">
                        版本文件
                      </p>
                    ) : null}
                    {group.files.map((file) => (
                      <FileRow
                        key={`${file.source}:${file.fileId}`}
                        file={file}
                        onClick={() => onPickFile(file, group)}
                      />
                    ))}
                  </div>
                </div>
              </Collapse>
            </div>
          );
        })}
      </Card>
    </div>
  );
}

function FilterChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cx(
        'h-[22px] rounded-[5px] border px-2 text-[10.5px] transition-colors',
        active
          ? 'border-accent/60 bg-accent/15 text-ink'
          : 'border-line text-ink3 hover:border-line-strong hover:text-ink',
      )}
    >
      {children}
    </button>
  );
}

function FileRow({ file, onClick }: { file: ModFile; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex w-full items-center gap-2 border-b border-line px-3 py-1.5 text-left last:border-b-0 transition-colors hover:bg-hover"
    >
      <Download size={11} className="shrink-0 text-ink4" />
      <span className="min-w-0 flex-1 truncate text-[11.5px] text-ink2" title={file.name}>
        {file.name}
      </span>
      <span className="hidden min-w-0 max-w-[220px] shrink-0 truncate font-mono text-[10px] text-ink4 md:block" title={file.fileName}>
        {file.fileName}
      </span>
      {file.releaseType !== 'release' ? (
        <span
          className={cx(
            'shrink-0 rounded-[4px] px-1.5 py-[1px] text-[9.5px]',
            file.releaseType === 'beta'
              ? 'bg-amber-500/15 text-amber-600'
              : 'bg-red/15 text-red',
          )}
        >
          {file.releaseType === 'beta' ? 'Beta' : 'Alpha'}
        </span>
      ) : null}
      <span className="w-[64px] shrink-0 text-right font-mono text-[10px] text-ink4">
        {formatBytes(file.size)}
      </span>
      {/* CurseForge 的文件接口常年返回 downloadCount=0，显示「0」只会误导，
          所以没有下载量时整列留空（Modrinth 有数据时照常显示）。 */}
      <span className="w-[58px] shrink-0 text-right font-mono text-[10px] text-ink4">
        {file.downloads > 0 ? formatCount(file.downloads) : ''}
      </span>
      <span className="w-[74px] shrink-0 text-right text-[10px] text-ink4">
        {formatDate(file.publishedAt)}
      </span>
    </button>
  );
}

/* ------------------------------------------------------------------ *
 * 安装确认
 * ------------------------------------------------------------------ */

interface InstallConfirmProps {
  file: ModFile;
  group: ModVersionGroup;
  target: ModTarget | null;
  installed: ModDetail['project']['installed'];
  overwrite: boolean;
  onOverwriteChange: (value: boolean) => void;
  onOpenPage: (url: string) => void;
}

function InstallConfirm({
  file,
  group,
  target,
  installed,
  overwrite,
  onOverwriteChange,
  onOpenPage,
}: InstallConfirmProps) {
  const sameFile = installed?.fileName === file.fileName;
  return (
    <div className="flex flex-col gap-2.5 p-4">
      <div className="flex items-start gap-2.5">
        <span className="flex h-[28px] w-[28px] shrink-0 items-center justify-center rounded-[7px] bg-accent/15 text-accent">
          <Download size={14} />
        </span>
        <div className="min-w-0">
          <p className="truncate text-[12.5px] font-medium text-ink" title={file.name}>
            {file.name}
          </p>
          <p className="truncate font-mono text-[10.5px] text-ink4" title={file.fileName}>
            {file.fileName}
          </p>
        </div>
      </div>

      <dl className="grid grid-cols-[76px_minmax(0,1fr)] gap-y-1.5 text-[11.5px]">
        <dt className="text-ink4">安装到</dt>
        <dd className="min-w-0 break-all text-ink2">
          {target ? `${target.loaderLabel} ${target.gameVersion}` : '—'}
          {target ? <span className="ml-1 font-mono text-[10px] text-ink4">{target.modsDir}</span> : null}
        </dd>
        <dt className="text-ink4">模组版本</dt>
        <dd className="min-w-0 break-all text-ink2">
          {file.versionNumber || file.name}
          <span className="ml-1.5 text-ink4">{formatBytes(file.size)}</span>
        </dd>
        <dt className="text-ink4">适配</dt>
        <dd className="min-w-0 text-ink2">
          {group.loaderLabel} {group.gameVersion}
          {file.gameVersions.length > 0 ? (
            <span className="ml-1.5 font-mono text-[10px] text-ink4">
              {file.gameVersions.slice(0, 6).join(', ')}
            </span>
          ) : null}
        </dd>
        <dt className="text-ink4">发布</dt>
        <dd className="text-ink2">
          {file.releaseType === 'release' ? '正式版' : file.releaseType === 'beta' ? 'Beta' : 'Alpha'}
          <span className="ml-1.5 text-ink4">{formatDate(file.publishedAt)}</span>
        </dd>
      </dl>

      {installed ? (
        <div className="rounded-[7px] border border-amber-500/40 bg-amber-500/10 px-2.5 py-2 text-[11px] leading-[16px] text-ink2">
          <p className="flex items-start gap-1.5">
            <AlertTriangle size={11} className="mt-[2px] shrink-0 text-amber-500" />
            <span>
              这个模组已经装过了：<span className="font-mono text-[10.5px]">{installed.fileName}</span>
              {sameFile ? '（文件名与本次相同）' : '（文件名与本次不同）'}。
              继续安装会用新文件替换它。
            </span>
          </p>
          <label className="mt-2 flex cursor-pointer items-center gap-1.5 text-ink2">
            <input
              type="checkbox"
              checked={overwrite}
              onChange={(event) => onOverwriteChange(event.target.checked)}
              className="h-[13px] w-[13px] accent-[var(--color-accent)]"
            />
            允许覆盖已存在的文件
          </label>
        </div>
      ) : null}

      <button
        type="button"
        onClick={() => onOpenPage(file.officialUrl)}
        className="flex items-center gap-1 self-start text-[10.5px] text-ink4 hover:text-ink2"
      >
        <ExternalLink size={10} />
        查看文件直链
      </button>
    </div>
  );
}
