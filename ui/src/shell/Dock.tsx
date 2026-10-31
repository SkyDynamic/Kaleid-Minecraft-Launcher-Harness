import { usePluginEnabled } from '../store/app';
import { hasActiveDownloads, useDownloads } from '../store/downloads';
import { usePrefs } from '../store/prefs';
import { useUI } from '../store/ui';
import { formatBytes, formatPercent, formatSpeed } from '../lib/util';
import { useSlot } from './slots';

/**
 * 右栏。
 *
 * 上半是 Minecraft 新闻（内置 `news` 插件，默认开启），下半是真实的下载队列。
 * 关闭新闻插件时不保留任何占位：新闻容器直接不渲染，队列用 `flex-1` 填满。
 * 队列区域整体可点击，打开 DownloadQueueDialog 查看总进度与每个子项。
 */
export function Dock() {
  const newsEnabled = usePluginEnabled('news');
  const downloadsEnabled = usePluginEnabled('version-manager');
  const News = useSlot('dock.news');
  const snapshot = useDownloads((s) => s.snapshot);
  const error = useDownloads((s) => s.error);
  const dockWidth = usePrefs((s) => s.dockWidth);
  const setQueueOpen = useUI((s) => s.setQueueOpen);
  const jobs = snapshot?.jobs ?? [];
  const active = hasActiveDownloads(snapshot);
  const newsVisible = newsEnabled && Boolean(News);

  if (!newsVisible && !downloadsEnabled) return null;

  return (
    <aside
      style={{ width: dockWidth }}
      className="flex h-full shrink-0 flex-col overflow-hidden border-l border-line bg-rail"
    >
      {newsVisible && News ? (
        <div className="min-h-0 flex-1 overflow-y-auto pb-2.5">
          <News />
        </div>
      ) : null}

      {downloadsEnabled ? (
      <section
        role="button"
        tabIndex={0}
        aria-label="打开下载队列详情"
        onClick={() => setQueueOpen(true)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            setQueueOpen(true);
          }
        }}
        className={[
          'group flex min-h-0 cursor-pointer flex-col border-t border-line text-left outline-none transition-colors hover:bg-layer2/40 focus-visible:bg-layer2/50',
          newsVisible ? 'shrink-0 pb-2.5 pt-2.5' : 'flex-1 pb-2.5 pt-2.5',
        ].join(' ')}
      >
        <header className="flex items-center justify-between px-3 pb-2">
          <span className="text-[10px] font-semibold uppercase tracking-[0.1em] text-ink4">
            任务队列
          </span>
          <span className="font-mono text-[9.5px] leading-none text-ink4">
            {jobs.length > 0 ? `${jobs.length}` : '—'}
          </span>
        </header>

        {jobs.length === 0 ? (
          <div className="px-3 pb-1">
            <p className="text-[11px] leading-[16px] text-ink4">
              {error ? '下载服务不可用' : '还没有下载任务'}
            </p>
            {active ? null : (
              <p className="mt-1 text-[10px] leading-[14px] text-ink4/70">点击查看队列</p>
            )}
          </div>
        ) : (
          <div
            className={[
              'min-h-0 space-y-2.5 overflow-y-auto px-3',
              newsEnabled ? 'max-h-[172px]' : 'flex-1',
            ].join(' ')}
          >
            {jobs.map((job) => {
              const running = job.status === 'running' || job.status === 'queued' || job.status === 'resolving';
              const failed = job.status === 'failed';
              const cancelled = job.status === 'cancelled';
              return (
                <div key={job.id} className="anim-fade-up">
                  <div className="flex items-baseline justify-between gap-2">
                    <span className="truncate text-[11.5px] leading-[15px] text-ink2">
                      {job.versionName}
                    </span>
                    <span className="shrink-0 font-mono text-[9.5px] leading-none text-ink4">
                      {failed ? '失败' : cancelled ? '已取消' : formatPercent(job.percent)}
                    </span>
                  </div>
                  <div className="relative mt-1 h-[3px] w-full overflow-hidden rounded-full bg-track">
                    <div
                      className={[
                        'h-full rounded-full transition-[width] duration-500',
                        failed ? 'bg-red' : cancelled ? 'bg-ink4' : 'bg-accent',
                      ].join(' ')}
                      style={{ width: `${Math.max(failed || cancelled ? 100 : 2, job.percent)}%` }}
                    />
                    {running ? (
                      <div
                        aria-hidden="true"
                        className="anim-shimmer pointer-events-none absolute inset-y-0 left-0 w-full bg-[linear-gradient(90deg,transparent,rgba(255,255,255,.4),transparent)] bg-[length:60%_100%] bg-no-repeat"
                      />
                    ) : null}
                  </div>
                  <div className="mt-1 flex items-baseline justify-between gap-2 font-mono text-[9.5px] leading-none text-ink4">
                    <span className="truncate">
                      {job.error ?? job.current ?? job.status}
                    </span>
                    <span className="shrink-0">
                      {job.totalBytes > 0
                        ? `${formatBytes(job.doneBytes)} / ${formatBytes(job.totalBytes)}`
                        : formatBytes(job.doneBytes)}
                    </span>
                  </div>
                </div>
              );
            })}
          </div>
        )}

        <footer className="flex items-center justify-between gap-2 px-3 pt-2 font-mono text-[9.5px] leading-none text-ink4">
          <span>
            {snapshot && snapshot.activeWorkers > 0
              ? `${snapshot.activeWorkers}/${snapshot.concurrency} 线程`
              : `${snapshot?.concurrency ?? 0} 线程`}
          </span>
          <span className="flex items-center gap-1.5">
            {snapshot && snapshot.speedBps > 0 ? <span>{formatSpeed(snapshot.speedBps)}</span> : null}
            <span className="text-ink4/60 transition-colors group-hover:text-ink3">详情 →</span>
          </span>
        </footer>
      </section>
      ) : null}
    </aside>
  );
}
