import type { ReactNode } from 'react';
import { Download, Network, Settings, Upload } from 'lucide-react';
import type { ModHit } from '../bridge/types';
import { cx, formatCount, formatRelative } from '../lib/util';

export interface ModRowProps {
  hit: ModHit;
  onClick?: () => void;
  /** 右下角额外内容（详情页用来标「已安装」）。 */
  badge?: ReactNode;
  className?: string;
}

/**
 * 模组列表项。搜索列表与详情页的前置列表共用同一个组件，
 * 这样「样式一致」不是靠复制粘贴保证的。
 *
 * 三行结构：
 * 1. 名称（有译名时 `译名 | 原名`，原名灰体、悬停可见全名）；
 * 2. 类型标签 + 简介（截断到项宽）；
 * 3. 加载器/版本区间 · 下载量 · 最后更新 · 来源，各带一枚图标。
 */
export function ModRow({ hit, onClick, badge, className }: ModRowProps) {
  const translated = hit.translatedName?.trim();
  // 设置里把「Mod 列表注释」切成翻译后，后端会带 MCIM 译文。
  const description = (hit.translatedDescription?.trim() || hit.description).trim();
  const loaderLine = hit.gameRange ? `${hit.loadersLabel} ${hit.gameRange}` : hit.loadersLabel;

  return (
    <div
      role={onClick ? 'button' : undefined}
      tabIndex={onClick ? 0 : undefined}
      onClick={onClick}
      onKeyDown={
        onClick
          ? (event) => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                onClick();
              }
            }
          : undefined
      }
      className={cx(
        'flex w-full items-start gap-2.5 border-b border-line px-3 py-2 last:border-b-0',
        onClick && 'cursor-pointer transition-colors hover:bg-hover focus:outline-none focus-visible:bg-hover',
        className,
      )}
    >
      <span className="mt-[1px] flex h-[34px] w-[34px] shrink-0 items-center justify-center overflow-hidden rounded-[8px] border border-line bg-layer2">
        {hit.iconUrl ? (
          <img
            src={hit.iconUrl}
            alt=""
            loading="lazy"
            referrerPolicy="no-referrer"
            className="h-full w-full object-cover"
          />
        ) : (
          <span className="text-[13px] font-semibold text-ink4">
            {(translated ?? hit.name).slice(0, 1).toUpperCase()}
          </span>
        )}
      </span>

      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-baseline gap-1 text-[12.5px] leading-[17px]">
          {translated ? (
            <>
              <span className="truncate font-medium text-ink" title={translated}>
                {translated}
              </span>
              <span className="shrink-0 text-ink4">|</span>
              <span className="truncate text-ink3" title={hit.name}>
                {hit.name}
              </span>
            </>
          ) : (
            <span className="truncate font-medium text-ink" title={hit.name}>
              {hit.name}
            </span>
          )}
        </div>

        <div className="mt-[3px] flex min-w-0 items-center gap-1.5 text-[11px] leading-[15px] text-ink3">
          {hit.typeLabel ? (
            <span className="shrink-0 rounded-[4px] bg-hover px-1.5 py-[1px] text-[10px] leading-[13px] text-ink3">
              {hit.typeLabel}
            </span>
          ) : null}
          <span className="min-w-0 truncate" title={description}>
            {description || '暂无简介'}
          </span>
        </div>

        <div className="mt-[5px] grid grid-cols-[minmax(0,1.2fr)_minmax(0,0.7fr)_minmax(0,0.75fr)_auto] items-center gap-x-3 text-[10.5px] leading-[14px] text-ink4">
          <Meta icon={<Settings size={10} />} text={loaderLine} />
          <Meta icon={<Download size={10} />} text={formatCount(hit.downloads)} />
          <Meta icon={<Upload size={10} />} text={formatRelative(hit.updatedAt)} />
          <Meta icon={<Network size={10} />} text={hit.source === 'modrinth' ? 'Modrinth' : 'CurseForge'} />
        </div>
      </div>

      {badge ? <div className="shrink-0 self-center">{badge}</div> : null}
    </div>
  );
}

function Meta({ icon, text }: { icon: ReactNode; text: string }) {
  return (
    <span className="flex min-w-0 items-center gap-1" title={text}>
      <span className="shrink-0 text-ink4">{icon}</span>
      <span className="truncate">{text}</span>
    </span>
  );
}
