/**
 * 「切换游戏目录」。
 *
 * 这个弹窗**不再在启动时自动出现**：打开启动器一律用默认目录，开了就能直接启动。
 * 只有用户从左栏目录菜单点「切换游戏目录…」时才会打开；顺手也能改默认目录
 * （`记住为默认`）。
 */
import { useState } from 'react';
import { FolderOpen, Star } from 'lucide-react';
import { Dialog } from './Dialog';
import { Button } from '../primitives';
import { getBridge } from '../bridge';
import type { Root } from '../bridge/types';
import { useApp } from '../store/app';
import { useUI } from '../store/ui';
import { cx } from '../lib/util';

const KIND_LABELS: Record<Root['kind'], string> = {
  bare_minecraft: 'Minecraft 目录',
  parent_with_mc: '含 .minecraft',
  empty_folder: '待创建',
  ordinary_dir: '普通目录',
};

export function RootChooserDialog() {
  const open = useApp((s) => s.rootChooserOpen);
  const roots = useApp((s) => s.roots);
  const activeRootId = useApp((s) => s.activeRootId);
  const defaultRootId = useApp((s) => s.defaultRootId);
  const [selected, setSelected] = useState<string | null>(null);
  const [remember, setRemember] = useState(false);
  const [busy, setBusy] = useState(false);

  const chosen = selected ?? activeRootId ?? roots[0]?.id ?? null;

  const close = () => {
    useApp.setState({ rootChooserOpen: false });
    setSelected(null);
    setRemember(false);
  };

  const confirm = async () => {
    if (!chosen) {
      close();
      return;
    }
    setBusy(true);
    try {
      if (chosen !== activeRootId) await useApp.getState().selectRoot(chosen);
      if (remember) await useApp.getState().setDefaultRoot(chosen);
      close();
    } finally {
      setBusy(false);
    }
  };

  const pickFolder = async () => {
    try {
      const path = await getBridge().pickFolder?.();
      if (!path) return;
      const root = await useApp.getState().addRoot(path, undefined);
      setSelected(root.id);
    } catch (e) {
      useUI.getState().showToast(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <Dialog
      open={open}
      onClose={close}
      title="切换游戏目录"
      subtitle="只影响这一次的选择；勾上「记住为默认」之后，下次打开启动器就直接用这个目录。"
      width={520}
      footer={
        <div className="flex items-center justify-between gap-3">
          <Button variant="ghost" size="md" icon={<FolderOpen size={13} />} onClick={() => void pickFolder()}>
            添加目录
          </Button>
          <div className="flex items-center gap-2">
            <label className="flex cursor-pointer select-none items-center gap-1.5 text-[11.5px] text-ink3">
              <input
                type="checkbox"
                checked={remember}
                onChange={(e) => setRemember(e.target.checked)}
                className="h-[13px] w-[13px] accent-[var(--accent)]"
              />
              记住为默认
            </label>
            <Button variant="primary" size="md" disabled={busy || !chosen} onClick={() => void confirm()}>
              使用这个目录
            </Button>
          </div>
        </div>
      }
    >
      <div className="p-2">
        {roots.map((root) => {
          const isDefault = root.id === defaultRootId;
          const isChosen = root.id === chosen;
          return (
            <button
              key={root.id}
              type="button"
              onClick={() => setSelected(root.id)}
              className={cx(
                'mb-1 flex w-full items-center gap-2.5 rounded-[7px] border px-3 py-2 text-left transition-colors',
                isChosen
                  ? 'border-accent/60 bg-accent/10'
                  : 'border-transparent hover:border-line hover:bg-hover',
              )}
            >
              <span
                className={cx(
                  'grid h-[14px] w-[14px] shrink-0 place-items-center rounded-full border',
                  isChosen ? 'border-accent bg-accent' : 'border-line-strong',
                )}
              >
                {isChosen ? <span className="h-[5px] w-[5px] rounded-full bg-white" /> : null}
              </span>
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-1.5">
                  <span className="truncate text-[12.5px] text-ink">{root.alias}</span>
                  {isDefault ? (
                    <span className="flex shrink-0 items-center gap-0.5 rounded-[4px] bg-amber/15 px-1 text-[9.5px] leading-[15px] text-amber">
                      <Star size={8} className="fill-current" />
                      默认
                    </span>
                  ) : null}
                  <span className="shrink-0 rounded-[4px] border border-line px-1 text-[9.5px] leading-[15px] text-ink4">
                    {KIND_LABELS[root.kind] ?? root.kind}
                  </span>
                </span>
                <span className="mt-0.5 block truncate font-mono text-[10.5px] text-ink4">
                  {root.path}
                </span>
              </span>
            </button>
          );
        })}
      </div>
    </Dialog>
  );
}
