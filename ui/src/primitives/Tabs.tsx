import type { ReactNode } from 'react';
import { cx } from '../lib/util';

export interface TabItem {
  id: string;
  label: string;
  count?: number;
}

export interface TabsProps {
  items: TabItem[];
  value: string;
  onChange: (id: string) => void;
  className?: string;
}

export function Tabs({ items, value, onChange, className }: TabsProps) {
  return (
    <div
      role="tablist"
      className={cx('flex items-stretch gap-0.5 border-b border-line px-2', className)}
    >
      {items.map((item) => {
        const active = item.id === value;
        return (
          <button
            key={item.id}
            role="tab"
            type="button"
            aria-selected={active}
            onClick={() => onChange(item.id)}
            className={cx(
              'relative flex h-[36px] select-none items-center gap-1.5 px-2.5 text-[12.5px] transition-colors',
              active ? 'text-ink' : 'text-ink3 hover:text-ink2',
            )}
          >
            <span>{item.label}</span>
            {typeof item.count === 'number' ? (
              <span
                className={cx(
                  'font-mono text-[10px] leading-none',
                  active ? 'text-ink3' : 'text-ink4',
                )}
              >
                {item.count}
              </span>
            ) : null}
            <span
              className={cx(
                'absolute inset-x-2 bottom-0 h-[2px] rounded-t-[2px] transition-colors',
                active ? 'bg-accent' : 'bg-transparent',
              )}
            />
          </button>
        );
      })}
    </div>
  );
}

export interface TabStripProps {
  items: Array<{ id: string; label: ReactNode }>;
  value: string;
  onChange: (id: string) => void;
  tone?: 'default' | 'accent';
  className?: string;
}

/** Pill/segmented tab strip (used by the login card). */
export function PillTabs({ items, value, onChange, className }: TabStripProps) {
  return (
    <div
      role="tablist"
      className={cx(
        'inline-flex items-center gap-0.5 rounded-[9px] border border-line bg-sunken p-[3px]',
        className,
      )}
    >
      {items.map((item) => {
        const active = item.id === value;
        return (
          <button
            key={item.id}
            role="tab"
            type="button"
            aria-selected={active}
            onClick={() => onChange(item.id)}
            className={cx(
              'h-[26px] select-none rounded-[7px] px-3.5 text-[12px] font-medium transition-colors',
              active
                ? 'bg-accent text-white shadow-[0_1px_2px_rgba(0,0,0,.35)]'
                : 'text-ink3 hover:bg-hover hover:text-ink',
            )}
          >
            {item.label}
          </button>
        );
      })}
    </div>
  );
}
