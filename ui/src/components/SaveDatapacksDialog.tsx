/**
 * 存档的数据包管理。
 *
 * 只做「看 / 导入 / 删」三件事：启用状态来自 `level.dat`，而项目里没有 NBT
 * 写入器（手写重编码有毁档风险），所以启用/禁用一律交给游戏自己写。
 * 界面上把这一条说明白，比放一个点不动的开关诚实。
 */
import { useEffect, useState } from 'react';
import { FileArchive, Folder, Loader2, Plus, Trash2 } from 'lucide-react';
import { Dialog } from './Dialog';
import { Button, IconButton } from '../primitives';
import { getBridge } from '../bridge';
import type { DataPack } from '../bridge/types';
import { useUI } from '../store/ui';
import { formatBytes } from '../lib/util';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function SaveDatapacksDialog({
  rootId,
  versionId,
  save,
  onClose,
}: {
  rootId: string | null;
  versionId: string | null;
  /** `folder` 是存档目录名；`name` 只用于标题。 */
  save: { folder: string; name: string } | null;
  onClose: () => void;
}) {
  const showToast = useUI((s) => s.showToast);
  const [packs, setPacks] = useState<DataPack[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    if (!rootId || !versionId || !save) return;
    try {
      const list = await getBridge().invoke<DataPack[]>('saves_datapacks_list', {
        rootId,
        versionId,
        folder: save.folder,
      });
      setPacks(list);
      setError(null);
    } catch (e) {
      setError(errText(e));
      setPacks([]);
    }
  };

  useEffect(() => {
    if (!save) return;
    setPacks(null);
    void load();
  }, [save?.folder, rootId, versionId]);

  const importPack = async () => {
    if (!rootId || !versionId || !save) return;
    const files = await getBridge().pickFiles?.(['zip']);
    if (!files || files.length === 0) return;
    setBusy(true);
    try {
      for (const source of files) {
        await getBridge().invoke('saves_datapack_import', {
          rootId,
          versionId,
          folder: save.folder,
          source,
        });
      }
      showToast(`已导入 ${files.length} 个数据包`);
      await load();
    } catch (e) {
      showToast(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const removePack = async (pack: DataPack) => {
    if (!rootId || !versionId || !save) return;
    try {
      await getBridge().invoke('saves_datapack_delete', {
        rootId,
        versionId,
        folder: save.folder,
        fileName: pack.fileName,
      });
      showToast(`已删除 ${pack.fileName}`);
      await load();
    } catch (e) {
      showToast(errText(e));
    }
  };

  return (
    <Dialog
      open={save !== null}
      onClose={onClose}
      title={save ? `数据包 · ${save.name}` : '数据包'}
      width={540}
      footer={
        <div className="flex items-center justify-between gap-2">
          <span className="text-[10.5px] text-ink4">
            启用状态由游戏写在 level.dat 里，这里只读
          </span>
          <div className="flex gap-2">
            <Button variant="ghost" size="md" onClick={onClose}>
              关闭
            </Button>
            <Button
              variant="primary"
              size="md"
              icon={<Plus size={12} />}
              disabled={busy || !save}
              onClick={() => void importPack()}
            >
              导入 zip
            </Button>
          </div>
        </div>
      }
    >
      <div className="max-h-[54vh] overflow-y-auto p-3">
        {error ? (
          <p className="mb-2 rounded-[6px] border border-line bg-layer2 px-3 py-2 text-[11px] text-ink3">
            {error}
          </p>
        ) : null}
        {packs === null ? (
          <p className="flex items-center gap-2 px-1 py-3 text-[11.5px] text-ink4">
            <Loader2 size={13} className="animate-spin" />
            正在读取数据包…
          </p>
        ) : packs.length === 0 ? (
          <p className="px-1 py-3 text-[11.5px] text-ink4">
            这个存档还没有自定义数据包。原版自带的包不算在内。
          </p>
        ) : (
          <ul className="space-y-1">
            {packs.map((pack) => (
              <li
                key={pack.fileName}
                className="flex h-[36px] items-center gap-2 rounded-[5px] px-2 hover:bg-hover"
              >
                {pack.kind === 'zip' ? (
                  <FileArchive size={13} className="shrink-0 text-ink4" />
                ) : (
                  <Folder size={13} className="shrink-0 text-ink4" />
                )}
                <span className="min-w-0 flex-1 truncate font-mono text-[11.5px] text-ink2">
                  {pack.fileName}
                </span>
                {pack.description ? (
                  <span className="max-w-[140px] shrink-0 truncate text-[10.5px] text-ink4">
                    {pack.description}
                  </span>
                ) : null}
                {pack.packFormat !== null ? (
                  <span className="shrink-0 font-mono text-[10px] text-ink4">
                    fmt {pack.packFormat}
                  </span>
                ) : null}
                <span
                  className="shrink-0 rounded-[4px] border border-line bg-layer2 px-1.5 text-[10px] text-ink3"
                  title={
                    pack.enabled === null
                      ? 'level.dat 里没有记录，可能由游戏按默认规则处理'
                      : pack.enabled
                        ? '已在这个存档里启用'
                        : '已在这个存档里禁用'
                  }
                >
                  {pack.enabled === null ? '未知' : pack.enabled ? '已启用' : '未启用'}
                </span>
                <span className="w-[54px] shrink-0 text-right font-mono text-[10px] text-ink4">
                  {formatBytes(pack.sizeBytes)}
                </span>
                <IconButton label={`删除 ${pack.fileName}`} onClick={() => void removePack(pack)}>
                  <Trash2 size={12} />
                </IconButton>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Dialog>
  );
}
