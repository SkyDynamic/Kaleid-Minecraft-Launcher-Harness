import { useEffect, useMemo, useState } from 'react';
import { Copy, FolderOpen, RefreshCw } from 'lucide-react';
import { openDirInShell } from '../bridge';
import type { LogTail } from '../bridge/types';
import { Button, Card, IconButton } from '../primitives';
import { LogView } from '../components/LogView';
import { parseLog } from '../lib/logFormat';
import { useContent, useContentSync } from '../store/content';
import { useApp, usePluginEnabled } from '../store/app';
import { useUI } from '../store/ui';
import { cx, formatBytes, formatDateTime } from '../lib/util';

/** 一屏最多画多少行：尾部 512 KB 可能上万行，全画出来滚动会卡。 */
const MAX_LINES = 5000;

/**
 * 日志页：列出 `logs/` 下的文件，点一个在右边预览**尾部**。
 *
 * 只读尾部（默认 512 KB）而不是整个文件：`debug.log` 长到几百 MB 很常见，
 * 一次性读进来界面会卡死。被截断时顶部会明说"只显示最后 N KB"。
 *
 * 滚动只发生在两个内容框里（左侧文件列表、右侧正文），页面本身不滚：
 * 页签和工具栏永远待在原地，日志多长都只用框内的滚动条。
 */
export function Logs() {
  const contentEnabled = usePluginEnabled('game-content');
  useContentSync(contentEnabled);

  const logs = useContent((s) => s.logs);
  const dirs = useContent((s) => s.dirs);
  const loading = useContent((s) => s.loading);
  const error = useContent((s) => s.error);
  const refresh = useContent((s) => s.refresh);
  const readLog = useContent((s) => s.readLog);

  const versionId = useApp((s) => s.activeVersionId);
  const showToast = useUI((s) => s.showToast);

  const [active, setActive] = useState<string | null>(null);
  const [tail, setTail] = useState<LogTail | null>(null);
  const [reading, setReading] = useState(false);

  // 列表变了（切版本 / 刷新）：没选中就自动选中最新那个
  useEffect(() => {
    if (logs.length === 0) {
      setActive(null);
      setTail(null);
      return;
    }
    if (!active || !logs.some((l) => l.fileName === active)) {
      setActive(logs[0].fileName);
    }
  }, [logs, active]);

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setReading(true);
    void readLog(active)
      .then((value) => {
        if (!cancelled) setTail(value);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setTail(null);
        showToast(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setReading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [active, readLog, showToast]);

  // 上千行的解析只跟文本有关，别让它跟着每次重渲染跑一遍。
  const parsed = useMemo(() => (tail ? parseLog(tail.text, MAX_LINES) : null), [tail]);

  const copy = async () => {
    if (!tail) return;
    try {
      await navigator.clipboard.writeText(tail.text);
      showToast('已复制当前预览内容');
    } catch {
      showToast('复制失败，请手动选择文本');
    }
  };

  const openFolder = async () => {
    if (!dirs) return;
    const ok = await openDirInShell(dirs.logsDir);
    if (!ok) showToast(`目录：${dirs.logsDir}`);
  };

  if (!contentEnabled) {
    return (
      <div className="px-5 py-6 text-[12px] text-ink4">
        「本地内容」插件已停用，日志不可用。到「设置 → 插件」里重新启用即可。
      </div>
    );
  }

  if (!versionId) {
    return <div className="px-5 py-6 text-[12px] text-ink4">先在左侧选择一个版本。</div>;
  }

  const warn = parsed?.counts.warn ?? 0;
  const errs = parsed?.counts.error ?? 0;

  return (
    <div className="flex h-full min-h-0 gap-2 p-3">
      <div className="flex min-h-0 w-[248px] shrink-0 flex-col">
        <div className="mb-2 flex shrink-0 items-center gap-1.5 px-1">
          <span className="text-[12px] font-medium text-ink2">日志文件</span>
          <span className="font-mono text-[10.5px] text-ink4">{logs.length}</span>
          <IconButton label="打开 logs 文件夹" onClick={() => void openFolder()}>
            <FolderOpen size={13} />
          </IconButton>
          <IconButton label="重新读取" onClick={() => void refresh()}>
            <RefreshCw size={13} className={cx(loading && 'animate-spin')} />
          </IconButton>
        </div>
        <Card className="min-h-0 flex-1 overflow-y-auto">
          {logs.length === 0 ? (
            <p className="px-3 py-4 text-[11.5px] text-ink4">
              这个版本还没有日志。启动一次游戏后，这里会出现 <span className="font-mono">latest.log</span>。
            </p>
          ) : null}
          {logs.map((file) => (
            <button
              key={file.fileName}
              type="button"
              onClick={() => setActive(file.fileName)}
              className={cx(
                'flex w-full items-center gap-2 border-b border-line px-3 py-1.5 text-left last:border-b-0',
                active === file.fileName ? 'bg-hover' : 'hover:bg-hover',
              )}
            >
              <span className="min-w-0 flex-1">
                <span
                  className={cx(
                    'block truncate font-mono text-[11px]',
                    active === file.fileName ? 'text-ink' : 'text-ink2',
                  )}
                  title={file.fileName}
                >
                  {file.fileName}
                </span>
                <span className="block font-mono text-[9.5px] text-ink4">
                  {formatBytes(file.sizeBytes)} · {formatDateTime(file.modifiedAt)}
                </span>
              </span>
              {file.isLatest ? (
                <span className="shrink-0 rounded-[4px] border border-line bg-layer2 px-1 text-[9.5px] text-ink3">
                  最新
                </span>
              ) : null}
            </button>
          ))}
        </Card>
      </div>

      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="mb-2 flex shrink-0 items-center gap-2 px-1">
          <span className="min-w-0 truncate font-mono text-[11px] text-ink2">
            {active ?? '未选择日志'}
          </span>
          {tail ? (
            <span className="shrink-0 font-mono text-[10px] text-ink4">
              {tail.truncated
                ? `只显示最后 ${formatBytes(tail.shownBytes)} / 共 ${formatBytes(tail.totalBytes)}`
                : `${formatBytes(tail.totalBytes)}`}
            </span>
          ) : null}
          {errs > 0 ? (
            <span className="shrink-0 rounded-[4px] border border-red/40 bg-red/[0.08] px-1.5 text-[9.5px] font-medium text-red">
              错误 {errs}
            </span>
          ) : null}
          {warn > 0 ? (
            <span className="shrink-0 rounded-[4px] border border-amber/40 bg-amber/[0.08] px-1.5 text-[9.5px] font-medium text-amber">
              警告 {warn}
            </span>
          ) : null}
          <Button
            variant="secondary"
            size="sm"
            className="ml-auto h-[24px]"
            icon={<Copy size={11} />}
            disabled={!tail || reading}
            onClick={() => void copy()}
          >
            复制
          </Button>
        </div>

        {error ? (
          <div className="mb-2 shrink-0 rounded-[6px] border border-line bg-layer2 px-3 py-2 text-[11px] text-ink3">
            {error}
          </div>
        ) : null}

        <div className="min-h-0 flex-1 overflow-hidden rounded-[8px] border border-line bg-code">
          {/* 只有这块滚：横向给长行，纵向给整份日志。 */}
          <div className="h-full overflow-auto">
            {reading ? (
              <p className="px-3 py-4 text-[11px] text-ink4">正在读取…</p>
            ) : parsed ? (
              <LogView log={parsed} />
            ) : (
              <p className="px-3 py-4 text-[11px] text-ink4">选一个日志文件预览</p>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
