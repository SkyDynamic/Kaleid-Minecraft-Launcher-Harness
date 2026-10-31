import { memo } from 'react';
import type { LogLevel, LogSpan, LogTone, ParsedLog } from '../lib/logFormat';
import { cx } from '../lib/util';

/**
 * 日志正文渲染：已经解析好的行 → 带色调的 DOM。
 *
 * 两件事刻意为之：
 * 1. **横向不折行**（`whitespace-pre` + `w-max`）：日志里长 UUID、长路径折成一坨
 *    反而读不出来，要看得靠横向滚动。
 * 2. 行首留一条 2px 的级别色带：`warn` 琥珀、`error/fatal` 红。色带对**所有**行都
 *    占位（透明），所以整块文本的左边界是齐的，不会因为出现一条警告就整体错位。
 */

const TONE: Record<LogTone, string> = {
  time: 'text-ink4',
  thread: 'text-ink4',
  'lv-trace': 'text-ink4',
  'lv-debug': 'text-ink4',
  'lv-info': 'text-green',
  'lv-warn': 'text-amber',
  'lv-error': 'text-red',
  'lv-fatal': 'text-red',
  body: 'text-ink2',
  'body-dim': 'text-ink4',
  'body-warn': 'text-amber/90',
  'body-error': 'text-red/90',
  chat: 'text-accent',
  frame: 'text-ink4',
  cause: 'text-red/80',
  'ansi-black': 'text-ink4',
  'ansi-red': 'text-red',
  'ansi-green': 'text-green',
  'ansi-yellow': 'text-amber',
  'ansi-blue': 'text-accent',
  'ansi-magenta': 'text-violet',
  'ansi-cyan': 'text-cyan',
  'ansi-white': 'text-ink',
};

const LINE: Record<LogLevel, string> = {
  plain: 'border-l-transparent',
  trace: 'border-l-transparent',
  debug: 'border-l-transparent',
  info: 'border-l-transparent',
  warn: 'border-l-amber bg-amber/[0.06]',
  error: 'border-l-red bg-red/[0.07]',
  fatal: 'border-l-red bg-red/[0.12]',
};

const ROW = 'whitespace-pre border-l-2 py-[1px] pr-6 leading-[1.75] hover:bg-hover';

function Span({ span }: { span: LogSpan }) {
  return <span className={cx(TONE[span.tone], span.strong && 'font-semibold')}>{span.text}</span>;
}

export const LogView = memo(function LogView({ log }: { log: ParsedLog }) {
  if (log.totalLines === 0) {
    return <p className="px-3 py-4 text-[11px] text-ink4">（这个日志是空的）</p>;
  }

  return (
    <div className="min-w-full w-max px-0 py-1.5 font-mono text-[10.5px]">
      {log.omitted > 0 ? (
        <div className="mb-1 whitespace-pre border-l-2 border-l-transparent pl-2 pr-6 text-[10px] text-ink4">
          {`… 前面还有 ${log.omitted} 行没画出来（只显示最后 ${log.lines.length} 行）`}
        </div>
      ) : null}
      {log.lines.map((line, index) => (
        <div
          key={index}
          className={cx(
            ROW,
            LINE[line.level],
            // 堆栈续行缩进：缩进量写在**这里**而不是再叠一个 pl-6，
            // 两个 padding 类同时出现时谁生效取决于样式表顺序，靠不住。
            line.continuation ? 'pl-6' : 'pl-2',
            line.level === 'fatal' && 'font-semibold',
          )}
        >
          {line.spans.length === 0 ? ' ' : line.spans.map((span, i) => <Span key={i} span={span} />)}
        </div>
      ))}
    </div>
  );
});
