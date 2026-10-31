/**
 * 已启动实例的监控面板（概览页）。
 *
 * 定位是"任务管理器里那一小段"：看谁在跑、跑了多久、进程号多少，以及对它做三件事
 * ——结束进程、看本次日志、把排障包导成 zip。多开时这里就是唯一的入口，
 * 因为主界面只显示**当前选中版本**的状态。
 *
 * 已结束的会话留最近几条：刚崩完想导日志的人，第一步就是回到概览页找它。
 */
import { useEffect, useState } from 'react';
import { Download, FileText, Square } from 'lucide-react';
import { Button, Card, Label } from '../primitives';
import { Dot, type DotTone } from './Dot';
import { SessionLogDialog } from './SessionLogDialog';
import { useLaunch, launchIsBusy } from '../store/launch';
import { useUI } from '../store/ui';
import type { LaunchState } from '../bridge/types';
import { cx } from '../lib/util';

/** 已结束的会话保留几条：够回溯刚才崩的那次，又不会把面板撑长。 */
const KEEP_ENDED = 3;

function statusLabel(state: LaunchState): string {
  switch (state.phase) {
    case 'preparing':
      return state.stageLabel || '准备中…';
    case 'launching':
      return '等待游戏窗口…';
    case 'running':
      return '运行中';
    case 'exited':
      return state.crashed ? '崩溃退出' : '已退出';
    case 'failed':
      return '启动失败';
    case 'cancelled':
      return '已取消';
    default:
      return '空闲';
  }
}

function statusTone(state: LaunchState): DotTone {
  if (state.phase === 'running') return 'green';
  if (state.phase === 'preparing' || state.phase === 'launching') return 'accent';
  if (state.phase === 'failed' || state.crashed) return 'red';
  return 'gray';
}

function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) return `${hours}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

export function InstancePanel() {
  const sessions = useLaunch((s) => s.sessions);
  const error = useLaunch((s) => s.error);
  const stop = useLaunch((s) => s.stop);
  const exportZip = useLaunch((s) => s.exportZip);
  const refresh = useLaunch((s) => s.refresh);
  const showToast = useUI((s) => s.showToast);
  const [logFor, setLogFor] = useState<{ id: number; title: string } | null>(null);
  const [busy, setBusy] = useState<number | null>(null);

  // 别人（主界面启动、CLI）开的会话这里也要看得见，所以自己订阅。
  useEffect(() => useLaunch.getState().watch(), []);

  const running = sessions.filter(launchIsBusy);
  const ended = sessions.filter((s) => !launchIsBusy(s) && s.sessionId > 0).slice(-KEEP_ENDED).reverse();
  const shown = [...running, ...ended];

  const onStop = async (state: LaunchState) => {
    setBusy(state.sessionId);
    try {
      await stop(state.sessionId);
      showToast(`已结束 ${state.versionId}`);
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const onExport = async (sessionId: number) => {
    setBusy(sessionId);
    try {
      const archive = await exportZip(sessionId);
      if (!archive) return; // 用户取消了目录选择
      const mb = archive.bytes / 1024 / 1024;
      showToast(
        `已导出 ${archive.fileName}（${archive.files} 个文件，${
          mb >= 1 ? `${mb.toFixed(1)} MB` : `${Math.round(archive.bytes / 1024)} KB`
        }）`,
      );
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Card className="px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <Label>游戏实例</Label>
          {running.length > 0 ? (
            <span className="rounded-[4px] bg-accent/15 px-1.5 py-[1px] text-[10px] leading-[15px] text-accent">
              {running.length} 个运行中
            </span>
          ) : null}
        </div>
        <button
          type="button"
          onClick={() => void refresh()}
          className="text-[10.5px] text-ink4 hover:text-ink2"
        >
          刷新
        </button>
      </div>

      {error ? (
        <div className="mt-2 rounded-[6px] border border-line bg-layer2 px-3 py-2 text-[10.5px] text-ink3">
          {error}
        </div>
      ) : null}

      {shown.length === 0 && !error ? (
        <div className="mt-2 px-1 py-3 text-[11px] leading-[16px] text-ink4">
          还没有启动过游戏。点右上角「启动游戏」，这里会出现它的进程、日志和排障包入口。
        </div>
      ) : (
        <div className="mt-2 flex flex-col gap-1">
          {shown.map((state) => {
            const active = launchIsBusy(state);
            return (
              <div
                key={state.sessionId}
                className={cx(
                  'flex items-center gap-2 rounded-[6px] px-2 py-[6px]',
                  active ? 'bg-layer2' : 'bg-transparent',
                )}
              >
                <Dot tone={statusTone(state)} className={active ? 'animate-pulse' : undefined} />
                <div className="min-w-0 flex-1">
                  <div className="flex min-w-0 items-center gap-1.5">
                    <span className="truncate text-[11.5px] font-medium text-ink">
                      {state.versionId}
                    </span>
                    <span className="shrink-0 font-mono text-[10px] text-ink5">
                      #{state.sessionId}
                      {state.pid ? ` · PID ${state.pid}` : ''}
                    </span>
                  </div>
                  <div className="mt-[1px] truncate text-[10px] text-ink4">
                    {statusLabel(state)}
                    {active || state.phase === 'exited'
                      ? ` · ${formatElapsed(state.elapsedMs)}`
                      : ''}
                    {state.exitCode !== null && !active ? ` · 退出码 ${state.exitCode}` : ''}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<FileText size={11} />}
                    onClick={() =>
                      setLogFor({ id: state.sessionId, title: `本次日志 · ${state.versionId}` })
                    }
                  >
                    日志
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={<Download size={11} />}
                    disabled={busy === state.sessionId}
                    onClick={() => void onExport(state.sessionId)}
                  >
                    导出
                  </Button>
                  {active ? (
                    <Button
                      variant="secondary"
                      size="sm"
                      icon={<Square size={10} className="fill-current" />}
                      disabled={busy === state.sessionId}
                      onClick={() => void onStop(state)}
                    >
                      结束进程
                    </Button>
                  ) : null}
                </div>
              </div>
            );
          })}
        </div>
      )}

      <SessionLogDialog
        sessionId={logFor?.id ?? null}
        title={logFor?.title ?? '本次日志'}
        onClose={() => setLogFor(null)}
        onExport={(id) => {
          setLogFor(null);
          void onExport(id);
        }}
      />
    </Card>
  );
}
