/**
 * 崩溃报告弹窗。
 *
 * 措辞上刻意保守：关键词是"命中了什么"，疑似模组一律标「推测 / 可能 / 高度可能」。
 * 崩溃归因在现实里没有确定性答案（同一个报错可能来自前置库、Java 版本、甚至显卡驱动），
 * 所以这里把证据原文一起摊开，让用户自己判断，而不是给一个斩钉截铁的结论。
 */
import { useState } from 'react';
import { AlertTriangle, Copy, FileDown, FolderOpen } from 'lucide-react';
import { Dialog } from './Dialog';
import { Button } from '../primitives';
import { getBridge } from '../bridge';
import type { CrashExport, CrashReport, CrashSuspect } from '../bridge/types';
import { useUI } from '../store/ui';

const CONFIDENCE_STYLE: Record<CrashSuspect['confidence'], string> = {
  high: 'bg-red/15 text-red',
  medium: 'bg-amber/15 text-amber',
  low: 'bg-layer3 text-ink3',
};

const CONFIDENCE_LABEL: Record<CrashSuspect['confidence'], string> = {
  high: '高度可能',
  medium: '可能',
  low: '推测',
};

export function CrashDialog({
  report,
  onClose,
}: {
  report: CrashReport | null;
  onClose: () => void;
}) {
  const showToast = useUI((s) => s.showToast);
  const [exporting, setExporting] = useState(false);
  const [exported, setExported] = useState<CrashExport | null>(null);

  const runExport = async () => {
    setExporting(true);
    try {
      const result = await getBridge().invoke<CrashExport>('crash_export', {});
      setExported(result);
      showToast(`已导出 ${result.files.length} 个文件到 ${result.dir}`);
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    } finally {
      setExporting(false);
    }
  };

  const copyLog = async () => {
    if (!report) return;
    try {
      await navigator.clipboard.writeText(report.logTail);
      showToast('已复制崩溃前的日志');
    } catch {
      showToast('复制失败，请手动选中日志文本');
    }
  };

  const openDir = async () => {
    if (!exported) return;
    try {
      await getBridge().openDir?.(exported.dir);
    } catch (e) {
      showToast(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <Dialog
      open={report !== null}
      onClose={onClose}
      title="游戏崩溃了"
      width={620}
      footer={
        <div className="flex w-full items-center justify-between gap-2">
          <span className="truncate text-[10.5px] text-ink4">
            {exported ? `导出目录：${exported.dir}` : '导出含 latest.log、manifest.json、启动脚本等'}
          </span>
          <div className="flex shrink-0 gap-2">
            <Button variant="ghost" size="md" icon={<Copy size={12} />} onClick={() => void copyLog()}>
              复制日志
            </Button>
            {exported ? (
              <Button
                variant="secondary"
                size="md"
                icon={<FolderOpen size={12} />}
                onClick={() => void openDir()}
              >
                打开目录
              </Button>
            ) : null}
            <Button
              variant="primary"
              size="md"
              icon={<FileDown size={12} />}
              disabled={exporting}
              onClick={() => void runExport()}
            >
              {exporting ? '正在导出…' : '导出排障包'}
            </Button>
          </div>
        </div>
      }
    >
      <div className="max-h-[62vh] overflow-y-auto px-4 py-3 text-[12px] leading-[19px] text-ink2">
        <p className="flex items-start gap-1.5 text-[11.5px] leading-[17px] text-ink3">
          <AlertTriangle size={13} className="mt-0.5 shrink-0 text-amber" />
          下面的关键词与模组都是从日志里<strong className="font-medium text-ink2">推测</strong>出来的，
          不是确定结论。改完模组再启动一次是最快的验证方式。
        </p>

        <section className="mt-3">
          <h3 className="mb-1.5 text-[11px] font-medium text-ink3">命中的关键词</h3>
          {report && report.keywords.length > 0 ? (
            <div className="flex flex-wrap gap-1.5">
              {report.keywords.map((k) => (
                <span
                  key={k.id}
                  className="rounded-[4px] bg-layer3 px-1.5 py-[1px] text-[10.5px] text-ink2"
                  title={`规则 ${k.id}`}
                >
                  {k.label} · {k.count} 次
                </span>
              ))}
            </div>
          ) : (
            <p className="text-[11px] text-ink4">没有命中已知关键词，日志可能还没写出崩溃信息。</p>
          )}
        </section>

        <section className="mt-3">
          <h3 className="mb-1.5 text-[11px] font-medium text-ink3">
            疑似相关的模组
            <span className="ml-1 text-[10px] text-ink4">（推测 / 可能）</span>
          </h3>
          {report && report.suspects.length > 0 ? (
            <ul className="space-y-1.5">
              {report.suspects.map((s) => (
                <li key={s.fileName} className="rounded-[5px] border border-line bg-layer1 p-2">
                  <div className="flex items-center gap-1.5">
                    <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-ink">
                      {s.fileName}
                    </span>
                    <span
                      className={`shrink-0 rounded-[4px] px-1.5 py-[1px] text-[10px] ${
                        CONFIDENCE_STYLE[s.confidence]
                      }`}
                    >
                      {CONFIDENCE_LABEL[s.confidence]}
                    </span>
                  </div>
                  {s.evidence ? (
                    <pre className="mt-1 max-h-[80px] overflow-auto whitespace-pre-wrap break-all font-mono text-[10px] leading-[14px] text-ink4">
                      {s.evidence}
                    </pre>
                  ) : null}
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-[11px] text-ink4">
              日志里没有指向具体模组的行。常见原因：内存不足、Java 版本不对、加载器与游戏版本不匹配。
            </p>
          )}
        </section>

        <section className="mt-3">
          <h3 className="mb-1.5 text-[11px] font-medium text-ink3">
            崩溃前的日志
            {report?.crashFile ? (
              <span className="ml-1 font-mono text-[10px] text-ink4">（官方崩溃报告已找到）</span>
            ) : null}
          </h3>
          <pre className="max-h-[220px] overflow-auto whitespace-pre-wrap break-all rounded-[5px] border border-line bg-layer1 p-2 font-mono text-[10px] leading-[15px] text-ink3">
            {report?.logTail || '（没有日志）'}
          </pre>
        </section>
      </div>
    </Dialog>
  );
}
