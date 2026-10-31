/**
 * 版本管理动作的两个弹窗：删除版本、更新加载器。
 *
 * 都放在这一个文件里，因为它们共享同一份"点开之后先问后端要状态"的形态：
 * 弹窗本身不猜任何东西（不猜路径、不猜能不能更新），一律以宿主返回为准。
 */
import { useEffect, useState } from 'react';
import { AlertTriangle, Loader2, RefreshCw } from 'lucide-react';
import { Dialog } from './Dialog';
import { Button, Select } from '../primitives';
import { getBridge } from '../bridge';
import type { LoaderUpdateStatus, VersionDeleteOutcome, VersionSummary } from '../bridge/types';
import { useApp } from '../store/app';
import { useUI } from '../store/ui';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/* ------------------------------------------------------------------ *
 * 删除版本
 * ------------------------------------------------------------------ */

export function VersionDeleteDialog({
  rootId,
  version,
  onClose,
}: {
  rootId: string | null;
  version: VersionSummary | null;
  onClose: () => void;
}) {
  const showToast = useUI((s) => s.showToast);
  const refreshVersions = useApp((s) => s.refreshVersions);
  const [busy, setBusy] = useState(false);

  const confirm = async () => {
    if (!rootId || !version) return;
    setBusy(true);
    try {
      const outcome = await getBridge().invoke<VersionDeleteOutcome>('versions_delete', {
        rootId,
        versionId: version.id,
      });
      const freed = (outcome.freedBytes / (1024 * 1024)).toFixed(1);
      showToast(`已删除 ${outcome.versionId}，释放 ${freed} MB`);
      await refreshVersions();
      onClose();
    } catch (e) {
      showToast(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={version !== null}
      onClose={onClose}
      title="删除版本"
      width={460}
      footer={
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="md" onClick={onClose}>
            取消
          </Button>
          <Button variant="danger" size="md" disabled={busy} onClick={() => void confirm()}>
            {busy ? '正在删除…' : '删除'}
          </Button>
        </div>
      }
    >
      <div className="p-4 text-[12px] leading-[20px] text-ink2">
        <p>
          将删除版本 <span className="font-mono text-ink">{version?.id}</span> 的整个版本目录，
          连同它的版本设置一起。
        </p>
        <p className="mt-2 text-[11.5px] text-ink4">
          存档、模组、资源包不会跟着删——它们在自己的目录里（版本隔离开启时在版本目录下的
          <span className="font-mono"> saves/ mods/ </span>，那种情况会一起被删，请先自行备份）。
        </p>
      </div>
    </Dialog>
  );
}

/* ------------------------------------------------------------------ *
 * 更新加载器
 * ------------------------------------------------------------------ */

export function LoaderUpdateDialog({
  rootId,
  version,
  onClose,
}: {
  rootId: string | null;
  version: VersionSummary | null;
  onClose: () => void;
}) {
  const showToast = useUI((s) => s.showToast);
  const refreshVersions = useApp((s) => s.refreshVersions);
  const [status, setStatus] = useState<LoaderUpdateStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [target, setTarget] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!rootId || !version) return;
    let alive = true;
    setStatus(null);
    setError(null);
    setTarget(null);
    void (async () => {
      try {
        const next = await getBridge().invoke<LoaderUpdateStatus>('versions_loader_status', {
          rootId,
          versionId: version.id,
        });
        if (!alive) return;
        setStatus(next);
        setTarget(next.recommended ?? next.latest ?? next.candidates[0] ?? null);
      } catch (e) {
        if (alive) setError(errText(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [rootId, version?.id]);

  const apply = async () => {
    if (!rootId || !version || !target) return;
    setBusy(true);
    try {
      await getBridge().invoke('versions_update_loader', {
        rootId,
        versionId: version.id,
        version: target,
      });
      showToast('已开始下载并替换加载器，进度见下载队列');
      await refreshVersions();
      onClose();
    } catch (e) {
      showToast(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const isVanilla = status?.loader === 'vanilla';
  const options = (status?.candidates ?? []).map((v) => ({
    value: v,
    label: v === status?.recommended ? `${v}（推荐）` : v,
  }));

  return (
    <Dialog
      open={version !== null}
      onClose={onClose}
      title="更新加载器"
      width={470}
      footer={
        <div className="flex items-center justify-between gap-2">
          <span className="text-[11px] text-ink4">
            {status?.current ? `当前 ${status.current}` : '当前版本未知'}
          </span>
          <div className="flex gap-2">
            <Button variant="ghost" size="md" onClick={onClose}>
              取消
            </Button>
            <Button
              variant="primary"
              size="md"
              disabled={busy || !target || isVanilla || target === status?.current}
              onClick={() => void apply()}
            >
              {busy ? '正在处理…' : '更新'}
            </Button>
          </div>
        </div>
      }
    >
      <div className="p-4 text-[12px] leading-[20px] text-ink2">
        {version ? (
          <p className="mb-3 text-[11.5px] text-ink4">
            版本 <span className="font-mono text-ink3">{version.id}</span>
            {version.gameVersion ? ` · Minecraft ${version.gameVersion}` : ''}
          </p>
        ) : null}

        {error ? (
          <p className="flex items-start gap-1.5 text-[11.5px] leading-[18px] text-amber">
            <AlertTriangle size={13} className="mt-0.5 shrink-0" />
            {error}
          </p>
        ) : !status ? (
          <p className="flex items-center gap-2 text-[11.5px] text-ink4">
            <Loader2 size={13} className="animate-spin" />
            正在检查可用的加载器版本…
          </p>
        ) : isVanilla ? (
          <p className="text-[11.5px] leading-[18px] text-ink3">
            这个版本没有安装模组加载器，没有可以更新的东西。想用模组请新建一个带加载器的版本。
          </p>
        ) : (
          <div className="space-y-3">
            <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[11.5px]">
              <span className="text-ink4">加载器</span>
              <span className="text-ink2">{status.loader}</span>
              <span className="text-ink4">已安装</span>
              <span className="font-mono text-ink2">{status.current ?? '未知'}</span>
              <span className="text-ink4">最新</span>
              <span className="font-mono text-ink2">
                {status.latest ?? '未知'}
                {status.newerAvailable ? (
                  <span className="ml-1.5 rounded-[4px] bg-accent/15 px-1 text-[10px] text-accent">
                    有更新
                  </span>
                ) : null}
              </span>
            </div>
            <div>
              <p className="mb-1 text-[11.5px] text-ink4">目标版本</p>
              {options.length > 0 ? (
                <Select
                  value={target ?? options[0]?.value ?? ''}
                  options={options}
                  onChange={(value) => setTarget(value)}
                  ariaLabel="目标加载器版本"
                />
              ) : (
                <p className="text-[11.5px] text-ink4">没有拿到可选的版本列表</p>
              )}
            </div>
            <p className="flex items-start gap-1.5 text-[10.5px] leading-[16px] text-ink4">
              <RefreshCw size={11} className="mt-0.5 shrink-0" />
              更新只会替换加载器那一层（版本 JSON 与加载器库），客户端 jar 与资源不动；
              已装的模组按新加载器版本自行负责兼容性。
            </p>
          </div>
        )}
      </div>
    </Dialog>
  );
}
