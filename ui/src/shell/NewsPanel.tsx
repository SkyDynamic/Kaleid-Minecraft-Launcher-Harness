import { useEffect, useState } from 'react';
import { RefreshCw } from 'lucide-react';
import { openExternal } from '../bridge';
import type { NewsEntry } from '../bridge/types';
import { IconButton } from '../primitives';
import { useNews } from '../store/news';
import { cx } from '../lib/util';

function formatTime(timestamp: number): string {
  return new Date(timestamp).toLocaleTimeString('zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * 右栏只有 ~120px 放文字，"Minecraft for Windows" 这种原文分类会被截成
 * "Minecraft for Win…"，等于没写。这里压成一眼能分清的短标签。
 */
function shortCategory(category: string): string {
  const value = category.toLowerCase();
  if (value.includes('java')) return 'Java';
  if (value.includes('dungeons')) return 'Dungeons';
  if (value.includes('legends')) return 'Legends';
  if (value.includes('windows') || value.includes('bedrock')) return '基岩版';
  if (value.includes('earth')) return 'Earth';
  return category.replace(/^minecraft\s*:?\s*/i, '') || category;
}

function NewsThumb({ item }: { item: NewsEntry }) {
  const [failed, setFailed] = useState(false);

  if (!item.imageUrl || failed) {
    return (
      <div className="grid h-[34px] w-[56px] shrink-0 place-items-center rounded-[5px] border border-line bg-layer2 font-mono text-[10px] text-ink4">
        {shortCategory(item.category).slice(0, 1) || 'M'}
      </div>
    );
  }

  return (
    <img
      src={item.imageUrl}
      alt=""
      loading="lazy"
      referrerPolicy="no-referrer"
      onError={() => setFailed(true)}
      className="h-[34px] w-[56px] shrink-0 rounded-[5px] border border-line object-cover"
    />
  );
}

function NewsRow({ item, index }: { item: NewsEntry; index: number }) {
  const clickable = item.link.length > 0;
  const content = (
    <>
      <NewsThumb item={item} />
      <div className="min-w-0 flex-1">
        <div className="line-clamp-2 text-[11.5px] leading-[15px] text-ink2">{item.title}</div>
        <div className="mt-0.5 flex items-baseline gap-1.5 font-mono text-[9.5px] leading-none text-ink4">
          <span className="shrink-0">{item.date.slice(5)}</span>
          <span className="truncate">{shortCategory(item.category)}</span>
        </div>
      </div>
    </>
  );

  const className =
    'anim-fade-up flex w-full items-start gap-2 rounded-[6px] px-1.5 py-1.5 text-left transition-colors duration-150';

  if (!clickable) {
    return (
      <div className={className} style={{ animationDelay: `${index * 35}ms` }}>
        {content}
      </div>
    );
  }

  return (
    <button
      type="button"
      title={`${item.text || item.title}\n${item.link}`}
      onClick={() => void openExternal(item.link)}
      style={{ animationDelay: `${index * 35}ms` }}
      className={cx(className, 'hover:bg-hover')}
    >
      {content}
    </button>
  );
}

/**
 * 右栏上半：Minecraft 最近新闻。
 *
 * 三种状态都必须看得见：正常、离线（有缓存）、失败（无缓存）。
 * 失败时给"重试"，而不是悄悄留空——空面板会让人以为这里本来就没内容。
 */
export function NewsPanel() {
  const items = useNews((s) => s.items);
  const status = useNews((s) => s.status);
  const fetchedAt = useNews((s) => s.fetchedAt);
  const error = useNews((s) => s.error);
  const load = useNews((s) => s.load);

  useEffect(() => {
    void load();
  }, [load]);

  const loading = status === 'loading';
  const showSkeleton = loading && items.length === 0;
  const showError = status === 'error' && items.length === 0;

  return (
    <section className="shrink-0 px-3 pt-3.5">
      <header className="flex items-center justify-between gap-2 pb-2">
        <span className="text-[10px] font-medium uppercase tracking-[0.1em] text-ink4">
          Minecraft 新闻
        </span>
        <div className="flex items-center gap-1.5">
          {status === 'stale' ? (
            <span
              className="font-mono text-[9px] leading-none text-amber"
              title={error ?? '使用本地缓存'}
            >
              离线{fetchedAt ? ` · ${formatTime(fetchedAt)}` : ''}
            </span>
          ) : null}
          {items.length > 0 ? (
            <span className="font-mono text-[9.5px] leading-none text-ink4">{items.length}</span>
          ) : null}
          <IconButton label="刷新新闻" size={22} onClick={() => void load(true)} disabled={loading}>
            <RefreshCw size={11} className={loading ? 'animate-spin' : undefined} />
          </IconButton>
        </div>
      </header>

      {showSkeleton ? (
        <div className="space-y-2 px-1.5">
          {[0, 1, 2].map((i) => (
            <div key={i} className="flex items-start gap-2">
              <div className="h-[34px] w-[56px] shrink-0 rounded-[5px] bg-layer3" />
              <div className="flex-1 space-y-1.5 pt-1">
                <div className="h-[9px] w-[86%] rounded-full bg-layer3" />
                <div className="h-[8px] w-[52%] rounded-full bg-layer2" />
              </div>
            </div>
          ))}
        </div>
      ) : null}

      {showError ? (
        <div className="px-1.5 pb-1">
          <p className="m-0 text-[11px] leading-[15px] text-ink4">新闻暂时取不到。</p>
          <p className="m-0 mt-1 text-[10px] leading-[14px] text-ink4">{error}</p>
          <button
            type="button"
            onClick={() => void load(true)}
            className="mt-1.5 select-none text-[11px] text-accent transition-colors hover:text-accent-hover"
          >
            重试
          </button>
        </div>
      ) : null}

      {items.length > 0 ? (
        <div className="space-y-0.5">
          {items.map((item, index) => (
            <NewsRow key={item.id} item={item} index={index} />
          ))}
        </div>
      ) : null}
    </section>
  );
}
