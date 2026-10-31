import { useEffect, useMemo, useState } from 'react';
import {
  ArrowUpCircle,
  Check,
  Download,
  FolderOpen,
  Import,
  Loader2,
  RefreshCw,
  Search,
  Trash2,
} from 'lucide-react';
import { getBridge, openDirInShell } from '../bridge';
import type { LocalMod, ModUpdate, ModUpdateReport } from '../bridge/types';
import { Button, Card, IconButton, Input } from '../primitives';
import { ModIconCell, clearModIconCache } from '../components/ModIconCell';
import { DeleteDialog, type DeleteTarget } from '../components/DeleteDialog';
import { importInto, useContent, useContentSync } from '../store/content';
import { useApp, usePluginEnabled } from '../store/app';
import { useModDownload } from '../store/modDownload';
import { useUI } from '../store/ui';
import { cx, formatBytes } from '../lib/util';

/**
 * 模组页：读的是**磁盘上真实的 `mods/`**（位置跟随版本隔离）。
 *
 * 启停 = `.jar` ↔ `.jar.disabled` 改名，由宿主完成；界面只管把结果重新读回来。
 */
export function Mods() {
  const [query, setQuery] = useState('');
  const [pending, setPending] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<DeleteTarget | null>(null);
  // 更新检查：进入页面就开始（异步、不挡列表渲染），失败也不弹错误
  const [updateReport, setUpdateReport] = useState<ModUpdateReport | null>(null);
  const [checking, setChecking] = useState(false);
  const [updating, setUpdating] = useState<string | null>(null);
  const [dismissedUpdates, setDismissedUpdates] = useState(false);

  const contentEnabled = usePluginEnabled('game-content');
  useContentSync(contentEnabled);

  const mods = useContent((s) => s.mods);
  const dirs = useContent((s) => s.dirs);
  const loading = useContent((s) => s.loading);
  const error = useContent((s) => s.error);
  const refresh = useContent((s) => s.refresh);
  const setModEnabled = useContent((s) => s.setModEnabled);

  const versionId = useApp((s) => s.activeVersionId);
  const rootId = useApp((s) => s.activeRootId);
  const modDownloadEnabled = usePluginEnabled('mod-download');
  const openModDownload = useModDownload((s) => s.openDialog);
  const showToast = useUI((s) => s.showToast);

  // 拖进来的文件直接装（只有当前页面在挂载时才订阅，切走自动退订）
  useEffect(() => {
    const off = getBridge().onFileDrop?.((paths) => {
      const jars = paths.filter((p) => p.toLowerCase().endsWith('.jar'));
      if (jars.length === 0) {
        showToast('只能拖入 .jar 模组文件');
        return;
      }
      void importInto('mod', jars);
    });
    return () => off?.();
  }, [showToast]);

  const enabled = mods.filter((m) => m.enabled).length;
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return mods;
    return mods.filter((m) => `${m.displayName} ${m.fileName}`.toLowerCase().includes(q));
  }, [mods, query]);

  const toggle = async (mod: LocalMod) => {
    setPending(mod.fileName);
    try {
      await setModEnabled(mod.fileName, !mod.enabled);
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    } finally {
      setPending(null);
    }
  };

  // 进入模组页就异步查一次更新：慢（要逐个哈希问 Modrinth）所以不阻塞列表。
  useEffect(() => {
    if (!rootId || !versionId) return;
    let alive = true;
    setChecking(true);
    setDismissedUpdates(false);
    void (async () => {
      try {
        const report = await getBridge().invoke<ModUpdateReport>('mods_updates_check', {
          rootId,
          versionId,
        });
        if (alive) setUpdateReport(report);
      } catch {
        // 没网/被墙都只是"这次没查到"，不打断用户看模组列表
        if (alive) setUpdateReport(null);
      } finally {
        if (alive) setChecking(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [rootId, versionId]);

  const updateFor = (mod: LocalMod): ModUpdate | undefined =>
    updateReport?.updates.find((u) => u.fileName === mod.fileName);

  const applyUpdate = async (update: ModUpdate) => {
    setUpdating(update.fileName);
    try {
      await getBridge().invoke('mods_apply_update', { rootId, versionId, update });
      showToast(`已更新 ${update.latestFileName}（旧文件备份在 .kaleid-backup）`);
      clearModIconCache();
      await refresh();
      setUpdateReport((prev) =>
        prev
          ? { ...prev, updates: prev.updates.filter((u) => u.fileName !== update.fileName) }
          : prev,
      );
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    } finally {
      setUpdating(null);
    }
  };

  const applyAllUpdates = async () => {
    const list = updateReport?.updates ?? [];
    for (const update of list) {
      await applyUpdate(update);
    }
  };

  const importFromDialog = async () => {
    const bridge = getBridge();
    if (!bridge.pickFiles) {
      showToast('当前外壳不支持文件选择，可以直接把 .jar 拖进来');
      return;
    }
    try {
      const paths = await bridge.pickFiles(['jar']);
      if (paths.length > 0) await importInto('mod', paths);
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    }
  };

  const openFolder = async () => {
    if (!dirs) return;
    const ok = await openDirInShell(dirs.modsDir);
    if (!ok) showToast(`目录：${dirs.modsDir}`);
  };

  if (!contentEnabled) {
    return (
      <div className="px-5 py-6 text-[12px] text-ink4">
        「本地内容」插件已停用，模组列表不可用。到「设置 → 插件」里重新启用即可。
      </div>
    );
  }

  if (!versionId) {
    return <div className="px-5 py-6 text-[12px] text-ink4">先在左侧选择一个版本。</div>;
  }

  return (
    <div className="p-3">
      <div className="mb-2 flex flex-wrap items-center gap-2 px-1">
        <div className="relative w-[240px]">
          <Search
            size={11}
            className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-ink4"
          />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索模组"
            className="h-[26px] pl-6 text-[11.5px]"
            aria-label="搜索模组"
          />
        </div>
        <Button variant="secondary" size="sm" icon={<Import size={11} />} onClick={() => void importFromDialog()}>
          导入
        </Button>
        {modDownloadEnabled ? (
          <Button
            variant="secondary"
            size="sm"
            icon={<Download size={11} />}
            onClick={openModDownload}
            title="从 Modrinth / CurseForge 下载模组"
          >
            下载 Mod
          </Button>
        ) : null}
        <IconButton label="打开 mods 文件夹" onClick={() => void openFolder()}>
          <FolderOpen size={13} />
        </IconButton>
        <IconButton label="重新读取" onClick={() => void refresh()}>
          <RefreshCw size={13} className={cx(loading && 'animate-spin')} />
        </IconButton>
        <span className="ml-auto font-mono text-[10.5px] text-ink4">
          已启用 {enabled} / {mods.length}
        </span>
      </div>

      {error ? (
        <div className="mb-2 rounded-[6px] border border-line bg-layer2 px-3 py-2 text-[11px] text-ink3">
          {error}
        </div>
      ) : null}

      {/* 更新提示条：进页面查一次，查不到就不占位置 */}
      {checking ? (
        <div className="mb-2 flex items-center gap-2 rounded-[6px] border border-line bg-layer2 px-3 py-1.5 text-[11px] text-ink3">
          <Loader2 size={12} className="animate-spin" />
          正在检查模组更新…
        </div>
      ) : updateReport && updateReport.updates.length > 0 && !dismissedUpdates ? (
        <div className="mb-2 flex items-center gap-2 rounded-[6px] border border-accent/40 bg-accent/[0.07] px-3 py-1.5 text-[11px] text-ink2">
          <ArrowUpCircle size={13} className="shrink-0 text-accent" />
          <span className="min-w-0 flex-1 truncate">
            {updateReport.updates.length} 个模组有更新
            {updateReport.unknown.length > 0
              ? `（${updateReport.unknown.length} 个查不到来源）`
              : ''}
          </span>
          <Button size="sm" variant="primary" onClick={() => void applyAllUpdates()}>
            全部更新
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setDismissedUpdates(true)}>
            忽略
          </Button>
        </div>
      ) : null}

      <Card className="overflow-hidden">
        {filtered.length === 0 ? (
          <p className="px-3 py-4 text-[11.5px] text-ink4">
            {mods.length === 0
              ? '这个版本还没有模组：点「导入」选 .jar，或直接把文件拖进来。'
              : '没有匹配的模组'}
          </p>
        ) : null}
        {filtered.map((mod) => {
          const update = updateFor(mod);
          return (
          <div
            key={mod.fileName}
            className="group flex h-[36px] items-center gap-3 border-b border-line px-3 last:border-b-0 hover:bg-hover"
          >
            <button
              type="button"
              role="checkbox"
              aria-checked={mod.enabled}
              aria-label={`${mod.enabled ? '停用' : '启用'} ${mod.displayName}`}
              disabled={pending === mod.fileName}
              onClick={() => void toggle(mod)}
              className={cx(
                'flex h-[15px] w-[15px] shrink-0 items-center justify-center rounded-[4px] border transition-colors',
                mod.enabled
                  ? 'border-accent bg-accent text-white'
                  : 'border-line-strong text-transparent hover:border-ink4',
                pending === mod.fileName && 'opacity-40',
              )}
            >
              <Check size={10} strokeWidth={3} />
            </button>
            {rootId && versionId ? (
              <ModIconCell
                rootId={rootId}
                versionId={versionId}
                fileName={mod.fileName}
                hasIcon={mod.hasIcon}
                dimmed={!mod.enabled}
              />
            ) : null}
            <span
              className={cx(
                'w-[160px] shrink-0 truncate text-[12.5px]',
                mod.enabled ? 'text-ink2' : 'text-ink4',
              )}
              title={mod.name ?? mod.displayName}
            >
              {mod.displayName}
            </span>
            <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-ink4" title={mod.fileName}>
              {mod.fileName}
            </span>
            <span className="w-[72px] shrink-0 truncate text-right font-mono text-[10px] text-ink4">
              {mod.version ?? '—'}
            </span>
            <span className="w-[54px] shrink-0 text-right font-mono text-[10px] text-ink4">
              {formatBytes(mod.sizeBytes)}
            </span>
            {update ? (
              <button
                type="button"
                disabled={updating === update.fileName}
                onClick={() => void applyUpdate(update)}
                title={`${update.currentVersion ?? '当前版本'} → ${update.latestVersion}`}
                className="flex shrink-0 items-center gap-1 rounded-[4px] bg-accent/15 px-1.5 py-[1px] text-[10px] text-accent transition-colors hover:bg-accent/25 disabled:opacity-50"
              >
                {updating === update.fileName ? (
                  <Loader2 size={10} className="animate-spin" />
                ) : (
                  <ArrowUpCircle size={10} />
                )}
                更新
              </button>
            ) : null}
            <IconButton
              label={`删除 ${mod.displayName}`}
              className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
              onClick={() =>
                setDeleting({ kind: 'mod', file: mod.fileName, label: mod.displayName })
              }
            >
              <Trash2 size={12.5} />
            </IconButton>
          </div>
          );
        })}
      </Card>

      <p className="mt-2 px-1 text-[10.5px] text-ink4">
        停用只是把文件改名成 <span className="font-mono">.jar.disabled</span>，随时可以再启用。
        {dirs ? (
          <>
            {' '}
            目录：<span className="font-mono">{dirs.modsDir}</span>
          </>
        ) : null}
      </p>

      <DeleteDialog target={deleting} onClose={() => setDeleting(null)} />
    </div>
  );
}
