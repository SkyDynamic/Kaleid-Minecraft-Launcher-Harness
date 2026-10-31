/**
 * 启动进度覆盖层（PCL2 那种盖住界面的模态框）。
 *
 * 为什么用覆盖层而不是角落里的小提示：启动前的补全动辄几分钟，用户这段时间
 * **不该**还能点别的版本、改设置、删模组——PCL2 也是这么做的。覆盖层上只有
 * 三件事可做：看进度、取消、失败后关掉。
 *
 * 进度数字全部来自后端（`percent` 是五步加权算好的），这里只负责画，
 * 免得两边各算一套、一个 60% 一个 70%。
 */
import { useEffect, useState } from 'react';
import {
  AlertTriangle,
  Check,
  ChevronRight,
  Loader2,
  Minus,
  Play,
  X,
} from 'lucide-react';
import { Button } from '../primitives';
import type { LaunchState, LaunchStep } from '../bridge/types';
import { cx } from '../lib/util';

/**
 * 五步的图标（不是状态，是"这一步在干什么"）。
 *
 * `window` 那步要等游戏真的画出窗口，慢的时候十几秒，光有转圈会让人以为卡死，
 * 所以配的是"进度条本身在动 + 文字说明"。
 */
function StepIcon({ status }: { status: LaunchStep['status'] }) {
  if (status === 'completed') return <Check size={11} className="text-green" />;
  if (status === 'running') return <Loader2 size={11} className="animate-spin text-accent" />;
  if (status === 'skipped') return <Minus size={11} className="text-ink4" />;
  if (status === 'failed') return <X size={11} className="text-red" />;
  return <ChevronRight size={11} className="text-ink5" />;
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

/** 补全那一步的字节进度：`12.3 MB / 45.0 MB · 3 / 57 个文件`。 */
function repairDetail(step: LaunchStep): string | null {
  if (step.totalBytes <= 0) return null;
  const size = `${formatBytes(step.doneBytes)} / ${formatBytes(step.totalBytes)}`;
  if (step.filesTotal <= 0) return size;
  return `${size} · ${step.filesDone} / ${step.filesTotal} 个文件`;
}

export function LaunchOverlay({
  state,
  onCancel,
  onClose,
}: {
  /** 要显示的会话；`null` = 不显示。 */
  state: LaunchState | null;
  onCancel: (sessionId: number) => void;
  onClose: () => void;
}) {
  // 进度条要有"走过去了"的动画，所以把 percent 放大成 0..100 的百分比数字。
  const percent = Math.max(0, Math.min(1, state?.percent ?? 0));
  const [shown, setShown] = useState(percent);

  // 后端进度是跳变的（一次下载完一批），这里补个缓动，条形图就不会一顿一顿。
  useEffect(() => {
    const timer = window.setTimeout(() => setShown(percent), 60);
    return () => window.clearTimeout(timer);
  }, [percent]);

  if (!state) return null;

  const preparing = state.phase === 'preparing';
  const failed = state.phase === 'failed';
  const cancelled = state.phase === 'cancelled';
  const repairStep = state.steps.find((s) => s.id === 'repair');
  const repairInfo = repairStep ? repairDetail(repairStep) : null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 backdrop-blur-[2px]">
      <div className="anim-fade-in w-[560px] max-w-[calc(100vw-40px)] rounded-[10px] border border-line bg-bg shadow-2xl">
        {/* ---- 标题 ---- */}
        <div className="flex items-start justify-between gap-4 border-b border-line px-5 pb-3 pt-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <Play size={12} className="shrink-0 fill-current text-accent" />
              <h2 className="truncate text-[14px] font-semibold leading-[19px] text-ink">
                {failed ? '启动失败' : cancelled ? '已取消启动' : '正在启动游戏'}
              </h2>
            </div>
            <div className="mt-1 truncate font-mono text-[10.5px] leading-[14px] text-ink4">
              {state.versionId}
              {state.sessionId > 0 ? ` · 会话 #${state.sessionId}` : ''}
            </div>
          </div>
          <div className="shrink-0 pt-[1px] text-right">
            <div className="font-mono text-[19px] font-semibold leading-[22px] tabular-nums text-ink">
              {Math.round(shown * 100)}
              <span className="text-[11px] text-ink4">%</span>
            </div>
          </div>
        </div>

        {/* ---- 总进度条 ---- */}
        <div className="px-5 pt-4">
          <div className="h-[6px] overflow-hidden rounded-full bg-layer3">
            <div
              className={cx(
                'h-full rounded-full transition-[width] duration-300 ease-out',
                failed ? 'bg-red' : cancelled ? 'bg-ink4' : 'bg-accent',
              )}
              style={{ width: `${Math.max(2, shown * 100)}%` }}
            />
          </div>
          <div className="mt-2 flex items-baseline justify-between gap-3">
            <span className="truncate text-[11px] text-ink3">{state.stageLabel}</span>
            {repairInfo ? (
              <span className="shrink-0 font-mono text-[10px] tabular-nums text-ink4">
                {repairInfo}
              </span>
            ) : null}
          </div>
        </div>

        {/* ---- 五步清单 ---- */}
        <div className="mx-5 my-3 rounded-[8px] border border-line bg-layer1 py-1">
          {state.steps.map((step) => (
            <div
              key={step.id}
              className={cx(
                'flex items-center gap-2 px-3 py-[5px]',
                step.status === 'running' && 'bg-layer2/60',
              )}
            >
              <span className="flex h-[13px] w-[13px] shrink-0 items-center justify-center">
                <StepIcon status={step.status} />
              </span>
              <span
                className={cx(
                  'w-[86px] shrink-0 text-[11px]',
                  step.status === 'pending' ? 'text-ink4' : 'text-ink2',
                  step.status === 'failed' && 'text-red',
                )}
              >
                {step.label}
              </span>
              <span
                className="min-w-0 flex-1 truncate text-[10.5px] text-ink4"
                title={step.detail || undefined}
              >
                {step.detail}
              </span>
            </div>
          ))}
        </div>

        {/* ---- 失败/说明 ---- */}
        {failed && state.error ? (
          <div className="mx-5 mb-3 flex items-start gap-2 rounded-[6px] border border-red/30 bg-red/10 px-3 py-2">
            <AlertTriangle size={12} className="mt-[1px] shrink-0 text-red" />
            <span className="min-w-0 break-words text-[11px] leading-[16px] text-red">
              {state.error}
            </span>
          </div>
        ) : null}
        {state.notes.length > 0 && !failed ? (
          <div className="mx-5 mb-3 rounded-[6px] border border-line bg-layer2 px-3 py-2 text-[10.5px] leading-[15px] text-ink3">
            {state.notes[0]}
          </div>
        ) : null}

        {/* ---- 按钮 ---- */}
        <div className="flex items-center justify-between gap-3 border-t border-line px-5 py-3">
          <span className="min-w-0 truncate text-[10px] text-ink5">
            {preparing
              ? '补全缺失文件时不要关掉启动器'
              : failed || cancelled
                ? '排障包里有这次的完整日志'
                : '窗口出来后这里的进度会自动收起'}
          </span>
          <div className="flex shrink-0 gap-2">
            {preparing ? (
              <Button
                variant="secondary"
                size="md"
                icon={<X size={12} />}
                onClick={() => onCancel(state.sessionId)}
              >
                取消启动
              </Button>
            ) : (
              <Button
                variant={failed || cancelled ? 'primary' : 'secondary'}
                size="md"
                onClick={onClose}
              >
                {failed || cancelled ? '知道了' : '隐藏'}
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
