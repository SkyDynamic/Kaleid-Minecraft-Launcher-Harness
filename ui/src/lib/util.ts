/** Tiny class-name joiner (avoids pulling in clsx). */
export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(' ');
}

/** 4096 -> "4 GB" (keeps one decimal only when needed). */
export function formatGb(mb: number): string {
  const gb = mb / 1024;
  return `${Number.isInteger(gb) ? gb : gb.toFixed(1)} GB`;
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

export function formatSpeed(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return '—';
  return `${formatBytes(bytesPerSecond)}/s`;
}

export function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return '0%';
  return `${Math.max(0, Math.min(100, value)).toFixed(value >= 99.95 ? 0 : 1)}%`;
}

/**
 * 把"可能是秒、可能是毫秒、也可能是 ISO 串"的时间戳变成 `Date`。
 *
 * 后端的时间字段不全是同一套单位：`content` 里是 epoch **秒**，而浏览器 API
 * 与正则里常见的是**毫秒**。以 1e12 分界（2001-09-09 之后的"秒"都远小于它，
 * 而"毫秒"已经 1.7e12）既能认对，又不会把 2100 年（4102444800 秒）误判成毫秒。
 * 单位在边界上就该统一，这里只是兜底：任何一处漏了换算，也不至于显示成 5 万年后。
 */
/** 空值与非有限数都算"没有时间"，统一给一个破折号，别把 `NaN` 印到界面上。 */
function blank(value: string | number | null | undefined): boolean {
  if (value === null || value === undefined || value === '') return true;
  return typeof value === 'number' && !Number.isFinite(value);
}

function toDate(value: string | number): Date | null {
  if (typeof value === 'number') {
    return new Date(value < 1e12 ? value * 1000 : value);
  }
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date;
}

export function formatDateTime(value: string | number | null | undefined): string {
  if (blank(value)) return '—';
  const date = toDate(value as string | number);
  if (!date) return String(value);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}/${pad(date.getMonth() + 1)}/${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

/** 只到日期的短格式：`2026/10/30`。 */
export function formatDate(value: string | number | null | undefined): string {
  if (blank(value)) return '—';
  const date = toDate(value as string | number);
  if (!date) return String(value);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}/${pad(date.getMonth() + 1)}/${pad(date.getDate())}`;
}

/**
 * 「3 天前」这种相对时间。超过一年就直接给日期，否则读者还得自己算年份。
 * 模组列表的「最后更新」用它——绝对时间在小字号里太占地方。
 */
export function formatRelative(
  value: string | number | null | undefined,
  now: number = Date.now(),
): string {
  if (value === null || value === undefined || value === '') return '—';
  const time = typeof value === 'number' ? (value < 1e12 ? value * 1000 : value) : Date.parse(value);
  if (!Number.isFinite(time)) return String(value);
  const diff = now - time;
  if (diff < 0) return '刚刚';
  const minute = 60_000;
  const hour = 60 * minute;
  const day = 24 * hour;
  if (diff < minute) return '刚刚';
  if (diff < hour) return `${Math.floor(diff / minute)} 分钟前`;
  if (diff < day) return `${Math.floor(diff / hour)} 小时前`;
  if (diff < 30 * day) return `${Math.floor(diff / day)} 天前`;
  if (diff < 365 * day) return `${Math.floor(diff / (30 * day))} 个月前`;
  return formatDate(time);
}

/** 大数字中文缩写：`2.57 亿` / `3 万` / `8261`。 */
export function formatCount(value: number): string {
  if (!Number.isFinite(value) || value < 0) return '0';
  if (value >= 100_000_000) return `${(value / 100_000_000).toFixed(2).replace(/\.?0+$/, '')} 亿`;
  if (value >= 10_000) return `${(value / 10_000).toFixed(value >= 100_000 ? 0 : 1).replace(/\.0$/, '')} 万`;
  return String(Math.round(value));
}
