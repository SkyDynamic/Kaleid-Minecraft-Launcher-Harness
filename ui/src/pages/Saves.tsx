import { useState } from 'react';
import { FolderOpen, Package, RefreshCw, Trash2 } from 'lucide-react';
import { getBridge, openDirInShell } from '../bridge';
import { Card, IconButton } from '../primitives';
import { SaveDatapacksDialog } from '../components/SaveDatapacksDialog';
import type { SaveDeleteOutcome } from '../bridge/types';
import { useContent, useContentSync } from '../store/content';
import { useApp, usePluginEnabled } from '../store/app';
import { useUI } from '../store/ui';
import { cx, formatBytes, formatDateTime } from '../lib/util';

/**
 * 存档页：只读列表（`saves/<文件夹>/level.dat`）。
 *
 * 名称、游戏模式、最后游玩时间都从 NBT 里读——不看文件夹名，因为
 * 文件夹名是 `New World` 还是 `新的世界` 完全取决于玩家。
 */
export function Saves() {
  const contentEnabled = usePluginEnabled('game-content');
  useContentSync(contentEnabled);

  const saves = useContent((s) => s.saves);
  const dirs = useContent((s) => s.dirs);
  const loading = useContent((s) => s.loading);
  const error = useContent((s) => s.error);
  const refresh = useContent((s) => s.refresh);

  const versionId = useApp((s) => s.activeVersionId);
  const rootId = useApp((s) => s.activeRootId);
  const showToast = useUI((s) => s.showToast);
  const [selected, setSelected] = useState<string | null>(null);
  // 正在管理数据包的那个存档（`null` = 弹窗关闭）
  const [packSave, setPackSave] = useState<{ folder: string; name: string } | null>(null);
  // 待确认删除的存档：点一次不删，先问一句
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const deleteSave = async (folder: string) => {
    try {
      const outcome = await getBridge().invoke<SaveDeleteOutcome>('saves_delete', {
        rootId,
        versionId,
        folder,
      });
      showToast(`已删除存档 ${folder}，释放 ${formatBytes(outcome.freedBytes)}`);
      setPendingDelete(null);
      await refresh();
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    }
  };

  const openFolder = async (sub?: string) => {
    if (!dirs) return;
    const path = sub ? `${dirs.savesDir}\\${sub}` : dirs.savesDir;
    const ok = await openDirInShell(path);
    if (!ok) showToast(`目录：${path}`);
  };

  if (!contentEnabled) {
    return (
      <div className="px-5 py-6 text-[12px] text-ink4">
        「本地内容」插件已停用，存档列表不可用。到「设置 → 插件」里重新启用即可。
      </div>
    );
  }

  if (!versionId) {
    return <div className="px-5 py-6 text-[12px] text-ink4">先在左侧选择一个版本。</div>;
  }

  return (
    <div className="p-3">
      <div className="mb-2 flex items-center gap-2 px-1">
        <span className="text-[12px] font-medium text-ink2">存档</span>
        <span className="font-mono text-[10.5px] text-ink4">{saves.length} 个世界</span>
        <IconButton label="打开 saves 文件夹" onClick={() => void openFolder()}>
          <FolderOpen size={13} />
        </IconButton>
        <IconButton label="重新读取" onClick={() => void refresh()}>
          <RefreshCw size={13} className={cx(loading && 'animate-spin')} />
        </IconButton>
      </div>

      {error ? (
        <div className="mb-2 rounded-[6px] border border-line bg-layer2 px-3 py-2 text-[11px] text-ink3">
          {error}
        </div>
      ) : null}

      <Card className="overflow-hidden">
        {saves.length === 0 ? (
          <p className="px-3 py-4 text-[11.5px] text-ink4">
            这个版本还没有存档。进游戏新建一个世界，回到这里就能看到。
          </p>
        ) : null}
        {saves.map((save) => (
          <div
            key={save.folder}
            onClick={() => setSelected(save.folder === selected ? null : save.folder)}
            className={cx(
              'flex h-[40px] cursor-pointer items-center gap-3 border-b border-line px-3 last:border-b-0 hover:bg-hover',
              save.folder === selected && 'bg-hover',
            )}
            title={save.name}
          >
            <span
              aria-hidden="true"
              className={cx(
                'h-[18px] w-[18px] shrink-0 rounded-[3px] border border-line-strong',
                save.hasIcon
                  ? 'bg-[repeating-linear-gradient(45deg,var(--color-layer3)_0_3px,var(--color-layer2)_3px_6px)]'
                  : 'bg-layer2',
              )}
            />
            <span className="w-[180px] shrink-0 truncate text-[12.5px] text-ink2">
              {save.name}
            </span>
            <span className="shrink-0 rounded-[4px] border border-line bg-layer2 px-1.5 text-[10px] text-ink3">
              {save.gameMode}
            </span>
            <span className="w-[74px] shrink-0 truncate font-mono text-[10px] text-ink4">
              {save.mcVersion ?? '—'}
            </span>
            <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-ink4">
              {save.folder}
            </span>
            <span className="w-[108px] shrink-0 text-right font-mono text-[10px] text-ink4">
              {formatDateTime(save.lastPlayed)}
            </span>
            <span className="w-[58px] shrink-0 text-right font-mono text-[10px] text-ink4">
              {save.sizeCapped ? '≥ ' : ''}
              {formatBytes(save.sizeBytes)}
            </span>
            <IconButton
              label={`管理 ${save.name} 的数据包`}
              onClick={(e) => {
                e.stopPropagation();
                setPackSave({ folder: save.folder, name: save.name });
              }}
            >
              <Package size={12.5} />
            </IconButton>
            <IconButton
              label={`打开 ${save.name} 的文件夹`}
              onClick={(e) => {
                e.stopPropagation();
                void openFolder(save.folder);
              }}
            >
              <FolderOpen size={12.5} />
            </IconButton>
            <IconButton
              label={
                pendingDelete === save.folder
                  ? `再点一次确认删除 ${save.name}`
                  : `删除 ${save.name}`
              }
              className={cx(pendingDelete === save.folder && 'text-red')}
              onClick={(e) => {
                e.stopPropagation();
                if (pendingDelete === save.folder) {
                  void deleteSave(save.folder);
                } else {
                  setPendingDelete(save.folder);
                  window.setTimeout(() => setPendingDelete(null), 4000);
                }
              }}
            >
              <Trash2 size={12.5} />
            </IconButton>
          </div>
        ))}
      </Card>

      <SaveDatapacksDialog
        rootId={rootId}
        versionId={versionId}
        save={packSave}
        onClose={() => setPackSave(null)}
      />

      <p className="mt-2 px-1 text-[10.5px] text-ink4">
        名字、游戏模式与最后游玩时间来自 <span className="font-mono">level.dat</span>；
        「≥」表示世界目录太大，只统计了一部分。
        {dirs ? (
          <>
            {' '}
            目录：<span className="font-mono">{dirs.savesDir}</span>
          </>
        ) : null}
      </p>
    </div>
  );
}
