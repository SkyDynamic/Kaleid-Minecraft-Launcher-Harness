import { create } from 'zustand';
import type { NewsEntry } from '../bridge/types';

/**
 * Minecraft 新闻。
 *
 * 数据源是官方启动器用的那份新闻 JSON：`launchercontent.mojang.com/v2/news.json`。
 * 它 CORS 全开，所以界面可以直接读；宿主侧不引入任何 HTTP 依赖。
 * 缓存进 localStorage，断网时显示上次的结果（并明确标成离线），
 * 没有缓存就老老实实报错——不拿假数据顶替真新闻。
 */

const FEED_URL = 'https://launchercontent.mojang.com/v2/news.json';
const IMAGE_BASE = 'https://launchercontent.mojang.com';
const CACHE_KEY = 'kaleid.news.v1';
const TTL = 30 * 60 * 1000;
const MAX_ITEMS = 5;
/** 至少留几条 Java 版新闻的位置（见 pickEntries）。 */
const JAVA_MIN = 2;
const TIMEOUT_MS = 8000;

export type NewsStatus = 'idle' | 'loading' | 'ready' | 'stale' | 'error';

interface CacheShape {
  items: NewsEntry[];
  fetchedAt: number;
}

interface RawEntry {
  id?: unknown;
  title?: unknown;
  category?: unknown;
  date?: unknown;
  text?: unknown;
  readMoreLink?: unknown;
  newsPageImage?: { url?: unknown } | null;
}

const asString = (value: unknown): string => (typeof value === 'string' ? value : '');

function absoluteImage(raw: RawEntry): string | null {
  const path = asString(raw.newsPageImage?.url);
  if (!path) return null;
  if (/^https?:\/\//i.test(path)) return path;
  return `${IMAGE_BASE}${path.startsWith('/') ? '' : '/'}${path}`;
}

/** 只保留能显示的条目：没有标题或日期的直接跳过，不占位。 */
function mapEntries(payload: unknown): NewsEntry[] {
  const entries =
    payload && typeof payload === 'object' && Array.isArray((payload as { entries?: unknown }).entries)
      ? ((payload as { entries: RawEntry[] }).entries as RawEntry[])
      : [];

  const mapped: NewsEntry[] = [];
  for (const raw of entries) {
    const title = asString(raw.title).trim();
    const date = asString(raw.date).trim();
    if (!title || !date) continue;
    mapped.push({
      id: asString(raw.id) || `${date}:${title}`,
      title,
      category: asString(raw.category).trim() || 'Minecraft',
      date,
      text: asString(raw.text).trim(),
      imageUrl: absoluteImage(raw),
      link: asString(raw.readMoreLink).trim(),
    });
  }

  mapped.sort((a, b) => b.date.localeCompare(a.date));
  return pickEntries(mapped);
}

/**
 * 官方新闻源里四分之三是基岩版商城推广，纯按日期取 5 条会让 Java 版启动器的
 * 新闻栏只剩商城广告。这里保证至少 `JAVA_MIN` 条 Java 版新闻，其余按日期补足，
 * 最后整体再按日期排序——展示上仍然是"新的在上"，不会看起来像排序坏了。
 */
function pickEntries(sorted: NewsEntry[]): NewsEntry[] {
  const java = sorted.filter((item) => /java/i.test(item.category)).slice(0, JAVA_MIN);
  const rest = sorted
    .filter((item) => !java.includes(item))
    .slice(0, Math.max(0, MAX_ITEMS - java.length));
  return [...java, ...rest].sort((a, b) => b.date.localeCompare(a.date));
}

function readCache(): CacheShape | null {
  if (typeof localStorage === 'undefined') return null;
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<CacheShape> | null;
    if (!parsed || !Array.isArray(parsed.items) || typeof parsed.fetchedAt !== 'number') return null;
    const items = parsed.items.filter(
      (item): item is NewsEntry =>
        !!item &&
        typeof item.title === 'string' &&
        typeof item.date === 'string' &&
        typeof item.link === 'string',
    );
    if (items.length === 0) return null;
    return { items: items.slice(0, MAX_ITEMS), fetchedAt: parsed.fetchedAt };
  } catch {
    return null;
  }
}

function writeCache(items: NewsEntry[], fetchedAt: number) {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify({ items, fetchedAt }));
  } catch {
    // 存不进去只影响下次启动，不影响这次显示。
  }
}

async function fetchFeed(): Promise<NewsEntry[]> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), TIMEOUT_MS);
  try {
    const response = await fetch(FEED_URL, {
      signal: controller.signal,
      headers: { Accept: 'application/json' },
    });
    if (!response.ok) throw new Error(`新闻源返回 HTTP ${response.status}`);
    const items = mapEntries(await response.json());
    if (items.length === 0) throw new Error('新闻源里没有可显示的条目');
    return items;
  } finally {
    clearTimeout(timer);
  }
}

const cached = readCache();
const cacheFresh = cached ? Date.now() - cached.fetchedAt < TTL : false;

interface NewsState {
  items: NewsEntry[];
  status: NewsStatus;
  fetchedAt: number | null;
  error: string | null;
  load: (force?: boolean) => Promise<void>;
}

export const useNews = create<NewsState>()((set, get) => ({
  items: cached?.items ?? [],
  status: cached ? (cacheFresh ? 'ready' : 'stale') : 'idle',
  fetchedAt: cached?.fetchedAt ?? null,
  error: null,

  load: async (force = false) => {
    const state = get();
    if (state.status === 'loading') return;

    const fresh = state.fetchedAt !== null && Date.now() - state.fetchedAt < TTL;
    if (!force && fresh && state.items.length > 0) return;

    set({ status: 'loading', error: null });
    try {
      const items = await fetchFeed();
      const fetchedAt = Date.now();
      writeCache(items, fetchedAt);
      set({ items, status: 'ready', fetchedAt, error: null });
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      // 有旧数据就降级展示（明确标"离线"），没有就报错。
      if (get().items.length > 0) {
        set({ status: 'stale', error: message });
      } else {
        set({ status: 'error', error: message });
      }
    }
  },
}));
