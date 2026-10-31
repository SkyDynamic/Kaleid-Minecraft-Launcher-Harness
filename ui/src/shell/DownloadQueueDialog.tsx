import { useEffect } from 'react';
import { Download, Loader2, XCircle } from 'lucide-react';
import { Dialog } from '../components/Dialog';
import { Button } from '../primitives';
import { useDownloads } from '../store/downloads';
import { useUI } from '../store/ui';
import { formatBytes, formatPercent, formatSpeed } from '../lib/util';
import type { DownloadJob, DownloadPhase, DownloadStatus } from '../bridge/types';

const STATUS_LABEL: Record<DownloadStatus, string> = {
  resolving: '解析中',
  queued: '等待中',
  running: '下载中',
  completed: '已完成',
  failed: '失败',
  cancelled: '已取消',
};

/** 任务来源标签：模组任务用 MCIM 镜像，别把它显示成 BMCLAPI。 */
const SOURCE_LABEL: Record<string, string> = {
  mojang: '官方',
  bmclapi: 'BMCLAPI',
  mcim: 'MCIM',
};

function statusTone(status: DownloadStatus): string {
  switch (status) {
    case 'completed':
      return 'border-green/30 bg-green/10 text-green';
    case 'failed':
      return 'border-red/30 bg-red/10 text-red';
    case 'cancelled':
      return 'border-line-strong bg-layer3 text-ink4';
    case 'running':
      return 'border-accent/30 bg-accent/10 text-accent-hover';
    default:
      return 'border-line-strong bg-layer2 text-ink3';
  }
}

function phasePercent(phase: DownloadPhase): number | null {
  if (phase.status === 'completed' || phase.status === 'skipped') return 100;
  if (phase.totalBytes > 0) return Math.min(100, (phase.doneBytes / phase.totalBytes) * 100);
  if (phase.filesTotal > 0) return Math.min(100, (phase.filesDone / phase.filesTotal) * 100);
  return null;
}

function PhaseRow({ phase }: { phase: DownloadPhase }) {
  const percent = phasePercent(phase);
  const indeterminate = phase.status === 'running' && percent === null;
  return (
    <div className="flex items-center gap-3 py-[5px]">
      <span className="w-[72px] shrink-0 truncate text-[10.5px] text-ink4">{phase.label}</span>
      <div className="relative h-[3px] min-w-0 flex-1 overflow-hidden rounded-full bg-track">
        {indeterminate ? (
          <div
            aria-hidden="true"
            className="anim-shimmer absolute inset-y-0 left-0 w-full bg-[linear-gradient(90deg,transparent,rgba(255,255,255,.5),transparent)] bg-[length:60%_100%] bg-no-repeat"
          />
        ) : (
          <div
            className="h-full rounded-full bg-accent transition-[width] duration-300"
            style={{ width: `${Math.max(percent ?? 0, phase.status === 'pending' ? 0 : 2)}%` }}
          />
        )}
      </div>
      <span className="w-[92px] shrink-0 text-right font-mono text-[9.5px] leading-none text-ink4">
        {indeterminate
          ? '安装中…'
          : phase.status === 'pending'
            ? '等待'
            : phase.status === 'skipped'
              ? '跳过'
              : percent === null
                ? '—'
                : `${formatPercent(percent)} · ${phase.filesTotal > 0 ? `${phase.filesDone}/${phase.filesTotal}` : formatBytes(phase.doneBytes)}`}
      </span>
    </div>
  );
}

function JobCard({ job }: { job: DownloadJob }) {
  const cancel = useDownloads((s) => s.cancel);
  const active = job.status === 'running' || job.status === 'queued' || job.status === 'resolving';
  return (
    <div className="rounded-[9px] border border-line bg-layer2/45 p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate text-[12.5px] font-medium leading-[17px] text-ink">
              {job.versionName}
            </span>
            <span
              className={`shrink-0 rounded-full border px-1.5 py-[1px] text-[9px] leading-[13px] ${statusTone(
                job.status,
              )}`}
            >
              {STATUS_LABEL[job.status]}
            </span>
          </div>
          <p className="mt-0.5 truncate text-[10.5px] leading-[15px] text-ink4">
            {job.gameVersion}
            {job.loaderVersion ? ` · ${job.loaderKind} ${job.loaderVersion}` : ''} · 来源{' '}
            {SOURCE_LABEL[job.source] ?? job.source}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <span className="font-mono text-[11px] text-ink2">{formatPercent(job.percent)}</span>
          {active ? (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void cancel(job.id)}
              title="取消这个下载任务"
            >
              <XCircle size={12} />
              取消
            </Button>
          ) : null}
        </div>
      </div>

      <div className="relative mt-2 h-[4px] w-full overflow-hidden rounded-full bg-track">
        <div
          className={[
            'h-full rounded-full transition-[width] duration-500',
            job.status === 'failed'
              ? 'bg-red'
              : job.status === 'cancelled'
                ? 'bg-ink4'
                : 'bg-accent',
          ].join(' ')}
          style={{ width: `${Math.max(job.percent, 2)}%` }}
        />
      </div>
      <div className="mt-1 flex items-center justify-between gap-3 font-mono text-[9.5px] leading-none text-ink4">
        <span className="truncate">{job.error ?? job.current}</span>
        <span className="shrink-0">
          {job.totalBytes > 0
            ? `${formatBytes(job.doneBytes)} / ${formatBytes(job.totalBytes)}`
            : formatBytes(job.doneBytes)}
        </span>
      </div>

      {job.error ? (
        <p className="mt-2 rounded-[6px] border border-red/25 bg-red/10 px-2 py-1.5 text-[10.5px] leading-[15px] text-red">
          {job.error}
        </p>
      ) : null}

      <div className="mt-2 border-t border-line/70 pt-1">
        {job.phases.map((phase) => (
          <PhaseRow key={phase.id} phase={phase} />
        ))}
      </div>
    </div>
  );
}

export function DownloadQueueDialog() {
  const open = useUI((s) => s.queueOpen);
  const setOpen = useUI((s) => s.setQueueOpen);
  const snapshot = useDownloads((s) => s.snapshot);
  const error = useDownloads((s) => s.error);
  const refresh = useDownloads((s) => s.refresh);
  const clearFinished = useDownloads((s) => s.clearFinished);

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  const jobs = snapshot?.jobs ?? [];
  const finished = jobs.filter(
    (job) => job.status === 'completed' || job.status === 'failed' || job.status === 'cancelled',
  ).length;

  return (
    <Dialog
      open={open}
      onClose={() => setOpen(false)}
      title="下载队列"
      subtitle="总进度、每个任务与各个安装阶段的实时状态"
      width={720}
      footer={
        <div className="flex items-center justify-between gap-3">
          <span className="text-[10.5px] text-ink4">
            {snapshot
              ? `${snapshot.activeJobs} 个进行中 · ${snapshot.queuedJobs} 个等待 · ${snapshot.activeWorkers}/${snapshot.concurrency} 线程`
              : '下载服务未就绪'}
          </span>
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              disabled={finished === 0}
              onClick={() => void clearFinished()}
            >
              清理已结束
            </Button>
            <Button variant="secondary" size="sm" onClick={() => setOpen(false)}>
              关闭
            </Button>
          </div>
        </div>
      }
    >
      <div className="space-y-3 p-4">
        {snapshot ? (
          <div className="rounded-[9px] border border-line bg-layer2/55 p-3">
            <div className="flex items-center justify-between gap-3">
              <div className="flex items-center gap-2">
                <Download size={14} className="text-accent" />
                <span className="text-[12px] font-medium text-ink">总进度</span>
                <span className="text-[10.5px] text-ink4">
                  {jobs.length} 个任务
                </span>
              </div>
              <span className="font-mono text-[13px] text-ink">{formatPercent(snapshot.percent)}</span>
            </div>
            <div className="mt-2 h-[5px] w-full overflow-hidden rounded-full bg-track">
              <div
                className="h-full rounded-full bg-accent transition-[width] duration-500"
                style={{ width: `${Math.max(snapshot.percent, jobs.length > 0 ? 1 : 0)}%` }}
              />
            </div>
            <div className="mt-1.5 flex items-center justify-between gap-3 font-mono text-[10px] text-ink4">
              <span>
                {formatBytes(snapshot.doneBytes)} / {formatBytes(snapshot.totalBytes)}
              </span>
              <span className="flex items-center gap-3">
                <span>{formatSpeed(snapshot.speedBps)}</span>
                <span>
                  {snapshot.activeWorkers}/{snapshot.concurrency} 线程
                </span>
              </span>
            </div>
          </div>
        ) : (
          <div className="rounded-[9px] border border-line bg-layer2/45 p-3 text-[11.5px] text-ink4">
            <Loader2 size={13} className="mr-2 inline animate-spin" />
            {error ?? '正在读取下载状态…'}
          </div>
        )}

        {jobs.length === 0 ? (
          <div className="rounded-[9px] border border-dashed border-line px-4 py-8 text-center">
            <p className="text-[12px] text-ink3">队列是空的</p>
            <p className="mt-1 text-[10.5px] text-ink4">从「获取新版本」开始安装后，这里会显示每个文件的进度。</p>
          </div>
        ) : (
          <div className="space-y-2.5">
            {jobs.map((job) => (
              <JobCard key={job.id} job={job} />
            ))}
          </div>
        )}
      </div>
    </Dialog>
  );
}
