import { useEffect, useMemo, useState } from 'react';
import { Check, FolderOpen, Import, RefreshCw, Search, Trash2 } from 'lucide-react';
import { getBridge, openDirInShell } from '../bridge';
import { Button, Card, IconButton, Input } from '../primitives';
import { DeleteDialog, type DeleteTarget } from '../components/DeleteDialog';
import { importInto, useContent, useContentSync } from '../store/content';
import { useApp, usePluginEnabled } from '../store/app';
import { useUI } from '../store/ui';
import { cx, formatBytes } from '../lib/util';

/**
 * 资源包页**只读启用状态**。
 *
 * 启用顺序与启用与否都写在游戏的 `options.txt` 里（`resourcePacks:[...]`），
 * 那个文件由游戏自己维护，启动器改了它很容易被游戏覆盖或在下次启动时错乱。
 * 所以这里只显示 + 提供导入/删除，勾选框是**禁用**的并标注原因。
 */
export function ResourcePacks() {
  const [query, setQuery] = useState('');
  const [deleting, setDeleting] = useState<DeleteTarget | null>(null);

  const contentEnabled = usePluginEnabled('game-content');
  useContentSync(contentEnabled);

  const packs = useContent((s) => s.packs);
  const dirs = useContent((s) => s.dirs);
  const loading = useContent((s) => s.loading);
  const error = useContent((s) => s.error);
  const refresh = useContent((s) => s.refresh);

  const versionId = useApp((s) => s.activeVersionId);
  const showToast = useUI((s) => s.showToast);

  useEffect(() => {
    const off = getBridge().onFileDrop?.((paths) => {
      const zips = paths.filter((p) => p.toLowerCase().endsWith('.zip'));
      if (zips.length === 0) {
        showToast('只能拖入 .zip 资源包');
        return;
      }
      void importInto('resource_pack', zips);
    });
    return () => off?.();
  }, [showToast]);

  const enabled = packs.filter((p) => p.enabled).length;
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return packs;
    return packs.filter((p) => `${p.displayName} ${p.fileName}`.toLowerCase().includes(q));
  }, [packs, query]);

  const importFromDialog = async () => {
    const bridge = getBridge();
    if (!bridge.pickFiles) {
      showToast('当前外壳不支持文件选择，可以直接把 .zip 拖进来');
      return;
    }
    try {
      const paths = await bridge.pickFiles(['zip']);
      if (paths.length > 0) await importInto('resource_pack', paths);
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    }
  };

  const openFolder = async () => {
    if (!dirs) return;
    const ok = await openDirInShell(dirs.resourcePacksDir);
    if (!ok) showToast(`目录：${dirs.resourcePacksDir}`);
  };

  if (!contentEnabled) {
    return (
      <div className="px-5 py-6 text-[12px] text-ink4">
        「本地内容」插件已停用，资源包列表不可用。到「设置 → 插件」里重新启用即可。
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
            placeholder="搜索资源包"
            className="h-[26px] pl-6 text-[11.5px]"
            aria-label="搜索资源包"
          />
        </div>
        <Button variant="secondary" size="sm" icon={<Import size={11} />} onClick={() => void importFromDialog()}>
          导入
        </Button>
        <IconButton label="打开 resourcepacks 文件夹" onClick={() => void openFolder()}>
          <FolderOpen size={13} />
        </IconButton>
        <IconButton label="重新读取" onClick={() => void refresh()}>
          <RefreshCw size={13} className={cx(loading && 'animate-spin')} />
        </IconButton>
        <span className="ml-auto font-mono text-[10.5px] text-ink4">
          {enabled} / {packs.length} 已启用
        </span>
      </div>

      {error ? (
        <div className="mb-2 rounded-[6px] border border-line bg-layer2 px-3 py-2 text-[11px] text-ink3">
          {error}
        </div>
      ) : null}

      <Card className="overflow-hidden">
        {filtered.length === 0 ? (
          <p className="px-3 py-4 text-[11.5px] text-ink4">
            {packs.length === 0
              ? '这个版本还没有资源包：点「导入」选 .zip，或直接把文件拖进来。'
              : '没有匹配的资源包'}
          </p>
        ) : null}
        {filtered.map((pack) => (
          <div
            key={pack.fileName}
            className="group flex h-[36px] items-center gap-3 border-b border-line px-3 last:border-b-0 hover:bg-hover"
          >
            <span
              role="img"
              aria-label={pack.enabled ? '已启用' : '未启用'}
              title="启用状态来自游戏的 options.txt（启动器只读）"
              className={cx(
                'flex h-[15px] w-[15px] shrink-0 items-center justify-center rounded-[4px] border',
                pack.enabled
                  ? 'border-accent/60 bg-accent/25 text-ink2'
                  : 'border-line-strong text-transparent',
              )}
            >
              <Check size={10} strokeWidth={3} />
            </span>
            <span
              className={cx(
                'w-[200px] shrink-0 truncate text-[12.5px]',
                pack.enabled ? 'text-ink2' : 'text-ink4',
              )}
              title={pack.displayName}
            >
              {pack.displayName}
            </span>
            <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-ink4" title={pack.fileName}>
              {pack.fileName}
            </span>
            {pack.isDir ? (
              <span className="shrink-0 rounded-[4px] border border-line bg-layer2 px-1.5 text-[10px] text-ink4">
                文件夹
              </span>
            ) : null}
            <span className="w-[54px] shrink-0 text-right font-mono text-[10px] text-ink4">
              {formatBytes(pack.sizeBytes)}
            </span>
            <IconButton
              label={`删除 ${pack.displayName}`}
              className="opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
              onClick={() =>
                setDeleting({ kind: 'resource_pack', file: pack.fileName, label: pack.displayName })
              }
            >
              <Trash2 size={12.5} />
            </IconButton>
          </div>
        ))}
      </Card>

      <p className="mt-2 px-1 text-[10.5px] text-ink4">
        启用与叠加顺序由游戏自己写在 <span className="font-mono">options.txt</span> 里，启动器只读取不修改；
        要调整请进游戏「选项 → 资源包」。删除后如果游戏正开着，会提示失败原因。
        {dirs ? (
          <>
            {' '}
            目录：<span className="font-mono">{dirs.resourcePacksDir}</span>
          </>
        ) : null}
      </p>

      <DeleteDialog target={deleting} onClose={() => setDeleting(null)} />
    </div>
  );
}
