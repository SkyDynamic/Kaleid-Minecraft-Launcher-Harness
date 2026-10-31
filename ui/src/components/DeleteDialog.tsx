import { useEffect, useState } from 'react';
import { Dialog } from './Dialog';
import { Button } from '../primitives';
import type { ContentKind } from '../bridge/types';
import { useContent } from '../store/content';

export interface DeleteTarget {
  kind: ContentKind;
  /** 磁盘上的文件名（模组是 `.jar` / `.jar.disabled`，资源包是 `.zip` 或目录名）。 */
  file: string;
  /** 界面上给人看的名字。 */
  label: string;
}

/**
 * 删除确认 / 删除失败。
 *
 * 为什么失败要单独弹窗而不是 toast：模组被游戏占用时删除会失败，用户需要
 * 看到**系统给的原因**（`os error 32` 之类），toast 两秒就没了，用户只会
 * 觉得"点了没反应"。所以同一个 Dialog 先确认、失败后原地变成失败说明。
 */
export function DeleteDialog({
  target,
  onClose,
}: {
  target: DeleteTarget | null;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 换目标（或重新打开）时清掉上一次的失败信息
  useEffect(() => {
    setError(null);
    setBusy(false);
  }, [target?.file, target?.kind]);

  const confirm = async () => {
    if (!target) return;
    setBusy(true);
    try {
      await useContent.getState().remove(target.kind, target.file);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const noun = target?.kind === 'mod' ? '模组' : '资源包';

  return (
    <Dialog
      open={target !== null}
      onClose={onClose}
      title={error ? `删除${noun}失败` : `删除${noun}`}
      width={480}
      footer={
        error ? (
          <div className="flex justify-end">
            <Button variant="primary" onClick={onClose}>
              知道了
            </Button>
          </div>
        ) : (
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={onClose} disabled={busy}>
              取消
            </Button>
            <Button variant="primary" onClick={() => void confirm()} disabled={busy}>
              {busy ? '正在删除…' : '删除'}
            </Button>
          </div>
        )
      }
    >
      <div className="p-4 text-[12px] leading-[20px] text-ink2">
        {error ? (
          <>
            <p className="m-0 text-ink">没能删除 {target?.label}。</p>
            <p className="m-0 mt-2 whitespace-pre-wrap break-all rounded-[6px] border border-line bg-layer2 px-2.5 py-2 font-mono text-[11px] text-ink3">
              {error}
            </p>
            <p className="m-0 mt-2 text-ink4">
              多半是游戏（或别的程序）正占用这个文件：先关掉游戏，再试一次。
            </p>
          </>
        ) : (
          <>
            <p className="m-0">
              将会从磁盘上删除 <span className="text-ink">{target?.label}</span>。
            </p>
            <p className="m-0 mt-2 break-all font-mono text-[11px] text-ink4">{target?.file}</p>
            <p className="m-0 mt-2 text-ink4">删除不可撤销，需要时只能重新下载或重新导入。</p>
          </>
        )}
      </div>
    </Dialog>
  );
}
