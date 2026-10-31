/**
 * 日志着色：把一行 MC / 启动器日志拆成"带色调的片段"。
 *
 * 认得的布局（按常见程度）：
 *   `[12:34:56] [Render thread/INFO]: 正文`            ← 原版 / Forge / Fabric 的 log4j 布局
 *   `[12:34:56] [main/INFO] [minecraft/MinecraftServer]: 正文` ← 带 logger 名
 *   `[12:34:56] [INFO] 正文` / `[INFO] 正文`           ← 启动器与部分 mod 自己的日志
 *   `\tat a.b.C(D.java:12)`、`Caused by: ...`、`... 12 more` ← 堆栈续行，缩进淡显
 *
 * 另外把 ANSI SGR（`\u001b[31m` 这类）翻成同一套色调，彩色输出不会变成一堆方括号乱码。
 *
 * 这里只做**显示**上的解析：除了 ANSI 控制码与行尾 `\r`，一个字都不改，
 * 「复制」拿到的仍是原文本。
 */

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error' | 'fatal' | 'plain';

export type LogTone =
  | 'time'
  | 'thread'
  | 'lv-trace'
  | 'lv-debug'
  | 'lv-info'
  | 'lv-warn'
  | 'lv-error'
  | 'lv-fatal'
  | 'body'
  | 'body-dim'
  | 'body-warn'
  | 'body-error'
  | 'chat'
  | 'frame'
  | 'cause'
  | 'ansi-black'
  | 'ansi-red'
  | 'ansi-green'
  | 'ansi-yellow'
  | 'ansi-blue'
  | 'ansi-magenta'
  | 'ansi-cyan'
  | 'ansi-white';

export interface LogSpan {
  text: string;
  tone: LogTone;
  strong?: boolean;
}

export interface LogLine {
  level: LogLevel;
  spans: LogSpan[];
  /** 堆栈续行（`at` / `Caused by` / `... n more`）：缩进显示，不参与级别统计。 */
  continuation: boolean;
}

export interface ParsedLog {
  lines: LogLine[];
  /** 文件里的总行数（渲染上并不一定全画出来）。 */
  totalLines: number;
  /** 因为行数上限被丢掉的行数。 */
  omitted: number;
  counts: { warn: number; error: number };
}

const LEVELS = ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR', 'FATAL'] as const;
type RawLevel = (typeof LEVELS)[number];

const LEVEL_OF: Record<RawLevel, LogLevel> = {
  TRACE: 'trace',
  DEBUG: 'debug',
  INFO: 'info',
  WARN: 'warn',
  ERROR: 'error',
  FATAL: 'fatal',
};

const LEVEL_TONE: Record<RawLevel, LogTone> = {
  TRACE: 'lv-trace',
  DEBUG: 'lv-debug',
  INFO: 'lv-info',
  WARN: 'lv-warn',
  ERROR: 'lv-error',
  FATAL: 'lv-fatal',
};

/** 正文的默认色调跟着级别走：debug 淡一点，warn/error 各用各的色。 */
function bodyTone(level: LogLevel): LogTone {
  switch (level) {
    case 'trace':
    case 'debug':
      return 'body-dim';
    case 'warn':
      return 'body-warn';
    case 'error':
    case 'fatal':
      return 'body-error';
    default:
      return 'body';
  }
}

const TS_RE = /^\[\d{1,2}:\d{2}:\d{2}(?:[.,]\d{1,6})?\]/;
const THREAD_LEVEL_RE = new RegExp(`^\\[([^\\]/]{0,80})/(${LEVELS.join('|')})\\]`);
const LEVEL_ONLY_RE = new RegExp(`^\\[(${LEVELS.join('|')})\\]`);
/** 没有方括号的宽松兜底：只用来定级别，不动文本。 */
const LOOSE_LEVEL_RE = new RegExp(`^[ \\t]*(?:\\[[^\\]]{0,40}\\][ \\t]*)?(${LEVELS.join('|')})[ \\t:：]`);
/** log4j 的 logger 名：`[minecraft/MinecraftServer]`。 */
const LOGGER_RE = /^[ \t]*\[[^\]]{0,80}\/(?:[^\]]{0,80})\]/;

const CHAT_RE = /^\[CHAT\][ \t]?/i;
const FRAME_RE = /^[ \t]+at[ \t]+\S/;
const MORE_RE = /^[ \t]*\.\.\.[ \t]+\d+[ \t]+more[ \t]*$/;
const CAUSE_RE = /^[ \t]*(?:Caused by|Suppressed):/;

/**
 * 非 SGR 的 CSI / OSC 转义：日志里偶尔混进清行、光标移动，直接丢掉。
 * 注意**不能**顺手把 `m` 结尾的序列也吃掉——那正是颜色本身。
 */
const OTHER_ESC_RE = /\u001b\[[0-9;?]*[A-Za-z]|\u001b\][^\u0007]*\u0007|\u001b[()][A-Z0-9]/g;

function stripNonSgr(text: string): string {
  return text.replace(OTHER_ESC_RE, (seq) => (seq.endsWith('m') ? seq : ''));
}
const SGR_RE = /\u001b\[([0-9;]*)m/g;

const ANSI_TONES: Record<number, LogTone> = {
  30: 'ansi-black',
  31: 'ansi-red',
  32: 'ansi-green',
  33: 'ansi-yellow',
  34: 'ansi-blue',
  35: 'ansi-magenta',
  36: 'ansi-cyan',
  37: 'ansi-white',
  90: 'ansi-black',
  91: 'ansi-red',
  92: 'ansi-green',
  93: 'ansi-yellow',
  94: 'ansi-blue',
  95: 'ansi-magenta',
  96: 'ansi-cyan',
  97: 'ansi-white',
};

/**
 * 把一段可能带 ANSI 颜色的文本切成同色调的片段。
 * 没有转义符时走快路径（绝大多数日志走这条）。
 */
export function splitAnsi(text: string, base: LogTone, strong = false): LogSpan[] {
  if (!text) return [];
  const cleaned = text.includes('\u001b') ? stripNonSgr(text) : text;
  if (!cleaned.includes('\u001b')) return [{ text: cleaned, tone: base, strong }];

  const out: LogSpan[] = [];
  let tone = base;
  let bold = strong;
  let cursor = 0;
  SGR_RE.lastIndex = 0;
  for (let m = SGR_RE.exec(cleaned); m; m = SGR_RE.exec(cleaned)) {
    const chunk = cleaned.slice(cursor, m.index);
    if (chunk) out.push({ text: chunk, tone, strong: bold });
    cursor = m.index + m[0].length;
    for (const part of m[1].split(';')) {
      const code = part === '' ? 0 : Number(part);
      if (code === 0) {
        tone = base;
        bold = strong;
      } else if (code === 1) {
        bold = true;
      } else if (code === 2) {
        bold = false;
      } else if (code === 22) {
        bold = strong;
      } else if (code === 39) {
        tone = base;
      } else if (ANSI_TONES[code]) {
        tone = ANSI_TONES[code];
      }
    }
  }
  const tail = cleaned.slice(cursor);
  if (tail) out.push({ text: tail, tone, strong: bold });
  return out;
}

function frameLine(raw: string): LogLine | null {
  if (CAUSE_RE.test(raw)) {
    return { level: 'plain', continuation: true, spans: [{ text: raw, tone: 'cause' }] };
  }
  if (MORE_RE.test(raw)) {
    return { level: 'plain', continuation: true, spans: [{ text: raw, tone: 'frame' }] };
  }
  if (FRAME_RE.test(raw)) {
    return { level: 'plain', continuation: true, spans: [{ text: raw, tone: 'frame' }] };
  }
  return null;
}

/** 解析一行。 */
export function parseLogLine(raw: string): LogLine {
  const text = raw.endsWith('\r') ? raw.slice(0, -1) : raw;
  const frame = frameLine(text);
  if (frame) return frame;

  let rest = text;
  let level: LogLevel = 'plain';
  let rawLevel: RawLevel | null = null;
  const spans: LogSpan[] = [];

  // 真实日志里各段之间是**有空格**的（`[12:00:00] [main/INFO]: …`），
  // 所以每吃掉一个前缀都要顺手吃掉它后面的空白，否则下一个前缀就不在行首了。
  // 空白本身留成片段，一个字都不改。
  const takeGap = () => {
    const m = rest.match(/^[ \t]+/);
    if (m && rest[m[0].length] === '[') {
      spans.push({ text: m[0], tone: 'time' });
      rest = rest.slice(m[0].length);
      return true;
    }
    return false;
  };

  takeGap();
  const ts = rest.match(TS_RE);
  if (ts) {
    spans.push({ text: ts[0], tone: 'time' });
    rest = rest.slice(ts[0].length);
    takeGap();
  }

  const threadLevel = rest.match(THREAD_LEVEL_RE);
  if (threadLevel) {
    rawLevel = threadLevel[2] as RawLevel;
    spans.push({ text: `[${threadLevel[1]}/`, tone: 'thread' });
    spans.push({ text: threadLevel[2], tone: LEVEL_TONE[rawLevel], strong: true });
    spans.push({ text: ']', tone: 'thread' });
    rest = rest.slice(threadLevel[0].length);
  } else {
    const onlyLevel = rest.match(LEVEL_ONLY_RE);
    if (onlyLevel) {
      rawLevel = onlyLevel[1] as RawLevel;
      spans.push({ text: '[', tone: 'thread' });
      spans.push({ text: onlyLevel[1], tone: LEVEL_TONE[rawLevel], strong: true });
      spans.push({ text: ']', tone: 'thread' });
      rest = rest.slice(onlyLevel[0].length);
    } else {
      const loose = rest.match(LOOSE_LEVEL_RE);
      if (loose) rawLevel = loose[1] as RawLevel;
    }
  }
  if (rawLevel) level = LEVEL_OF[rawLevel];

  // logger 名（`[minecraft/MinecraftServer]`）只在前缀已经出现时才吃掉，
  // 免得把普通消息开头的方括号当成了 logger。
  if (rawLevel) {
    takeGap();
    const logger = rest.match(LOGGER_RE);
    if (logger) {
      spans.push({ text: logger[0], tone: 'thread' });
      rest = rest.slice(logger[0].length);
    }
  }

  // log4j 的分隔冒号（`[main/INFO]: 消息`）连同后面的空白一起淡显，
  // 这样正文从真正的内容开始；没有冒号时退一步，把分隔空白也吃掉。
  if (rawLevel) {
    const sep = rest.match(/^[ \t]*[:：][ \t]*/);
    if (sep) {
      spans.push({ text: sep[0], tone: 'time' });
      rest = rest.slice(sep[0].length);
    } else {
      const gap = rest.match(/^[ \t]+/);
      if (gap) {
        spans.push({ text: gap[0], tone: 'time' });
        rest = rest.slice(gap[0].length);
      }
    }
  }

  if (spans.length === 0 && !rest) return { level, continuation: false, spans: [] };

  let tone = bodyTone(level);
  const chat = rest.match(CHAT_RE);
  if (chat) {
    spans.push({ text: chat[0].trimEnd(), tone: 'chat', strong: true });
    rest = rest.slice(chat[0].length);
    if (rest) spans.push({ text: ' ', tone });
    tone = 'body';
  }

  spans.push(...splitAnsi(rest, tone, level === 'fatal'));
  return { level, continuation: false, spans };
}

/**
 * 解析整份文本。`maxLines` 只限制**渲染**的行数（尾部优先），统计仍按全文算。
 */
export function parseLog(text: string, maxLines = 5000): ParsedLog {
  const raw = text.split('\n');
  // 文本以换行结尾时会多切出一个空串，不算一行。
  if (raw.length > 0 && raw[raw.length - 1] === '') raw.pop();

  const all = raw.map(parseLogLine);
  let warn = 0;
  let error = 0;
  for (const line of all) {
    if (line.continuation) continue;
    if (line.level === 'warn') warn += 1;
    else if (line.level === 'error' || line.level === 'fatal') error += 1;
  }

  const omitted = Math.max(0, all.length - maxLines);
  return {
    lines: omitted > 0 ? all.slice(omitted) : all,
    totalLines: all.length,
    omitted,
    counts: { warn, error },
  };
}
