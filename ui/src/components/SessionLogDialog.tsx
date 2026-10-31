/**
 * 「本次日志」弹窗：只看**这一次启动**的控制台输出。
 *
 * 和「日志」页签不是一回事：那里是磁盘上的 `logs/latest.log`（上一次游戏的），
 * 这里是启动器自己抄下来的 stdout/stderr，游戏还没写日志就崩了的时候只有它有内容。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { Copy, Download, FolderOpen, RefreshCw } from 'lucide-react';
import { Dialog } from './Dialog';
import { Button } from '../primitives';
import { getBridge } from '../bridge';
import type { LogTail } from '../bridge/types';
import { useUI } from '../store/ui';

/** 一次最多拉 256 KB：够看崩溃现场，也不会把界面卡住。 */
const MAX_BYTES = 256 * 1024;

export function SessionLogDialog({
  sessionId,
  title,
  onClose,
  onExport,
}: {
  /** `null` = 关闭。 */
  sessionId: number | null;
  title: string;
  onClose: () => void;
  /** 导排障包（父组件负责选目录 + zip）。 */
  onExport?: (sessionId: number) => void;
}) {
  const showToast = useUI((s) => s.showToast);
  const [tail, setTail] = useState<LogTail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [following, setFollowing] = useState(true);
  const boxRef = useRef<HTMLPreElement | null>(null);

  const load = useCallback(
    async (id: number) => {
      setLoading(true);
      try {
        const result = await getBridge().invoke<LogTail>('launch_console_tail', {
          sessionId: id,
          maxBytes: MAX_BYTES,
        });
        setTail(result);
        setError(null);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  // 打开时拉一次；选「跟随」就每秒刷新（游戏还在往里写）。
  useEffect(() => {
    if (sessionId === null) {
      setTail(null);
      setError(null);
      return;
    }
    void load(sessionId);
    if (!following) return;
    const timer = window.setInterval(() => void load(sessionId), 1000);
    return () => window.clearInterval(timer);
  }, [sessionId, following, load]);

  // 跟着滚到底部，最新的一行永远可见
  useEffect(() => {
    if (!following) return;
    const box = boxRef.current;
    if (box) box.scrollTop = box.scrollHeight;
  }, [tail?.text, following]);

  const copy = async () => {
    if (!tail?.text) return;
    try {
      await navigator.clipboard.writeText(tail.text);
      showToast('已复制本次日志');
    } catch {
      showToast('复制失败，请手动选中文本');
    }
  };

  return (
    <Dialog
      open={sessionId !== null}
      onClose={onClose}
      title={title}
      width={720}
      footer={
        <div className="flex w-full items-center justify-between gap-2">
          <span className="min-w-0 truncate text-[10.5px] text-ink4">
            {error
              ? error
              : tail
                ? `${tail.fileName} · 共 ${Math.round(tail.totalBytes / 1024)} KB${
                    tail.truncated ? '（只显示尾部）' : ''
                  }`
                : '还没有输出'}
          </span>
          <div className="flex shrink-0 gap-2">
            <Button
              variant="ghost"
              size="md"
              icon={<RefreshCw size={12} className={loading ? 'animate-spin' : undefined} />}
              onClick={() => sessionId !== null && void load(sessionId)}
            >
              刷新
            </Button>
            <Button variant="ghost" size="md" icon={<Copy size={12} />} onClick={() => void copy()}>
              复制
            </Button>
            {onExport && sessionId !== null ? (
              <Button
                variant="secondary"
                size="md"
                icon={<Download size={12} />}
                onClick={() => onExport(sessionId)}
              >
                导出排障包
              </Button>
            ) : null}
          </div>
        </div>
      }
    >
      <div className="flex flex-col gap-2">
        <label className="flex items-center gap-2 text-[10.5px] text-ink3">
          <input
            type="checkbox"
            checked={following}
            onChange={(e) => setFollowing(e.target.checked)}
            className="h-[12px] w-[12px] accent-[var(--accent)]"
          />
          跟随最新输出（每秒刷新）
        </label>
        <pre
          ref={boxRef}
          className="h-[380px] overflow-auto whitespace-pre-wrap break-words rounded-[6px] border border-line bg-layer1 p-3 font-mono text-[10.5px] leading-[16px] text-ink2"
        >
          {tail?.text || (error ? '' : '等待游戏输出…')}
        </pre>
      </div>
    </Dialog>
  );
}

/** 「打开日志文件」用的小按钮（概览面板里和结束进程并排）。 */
export function OpenLogButton({ path }: { path: string | null }) {
  const showToast = useUI((s) => s.showToast);
  return (
    <Button
      variant="ghost"
      size="sm"
      icon={<FolderOpen size={11} />}
      disabled={!path}
      onClick={() => {
        if (!path) return;
        void getBridge()
          .openDir?.(path)
          .catch((e) => showToast(e instanceof Error ? e.message : String(e)));
      }}
    >
      日志目录
    </Button>
  );
}
