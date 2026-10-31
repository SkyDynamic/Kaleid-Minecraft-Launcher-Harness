import type { ReactNode } from 'react';
import { cx } from '../lib/util';

export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
  disabled?: boolean;
}

export interface SegmentedControlProps<T extends string> {
  value: T;
  options: Array<SegmentedOption<T>>;
  onChange: (value: T) => void;
  tone?: 'default' | 'accent';
  size?: 'sm' | 'md';
  className?: string;
  ariaLabel?: string;
}

export function SegmentedControl<T extends string>({
  value,
  options,
  onChange,
  tone = 'default',
  size = 'md',
  className,
  ariaLabel,
}: SegmentedControlProps<T>) {
  return (
    <div
      role="tablist"
      aria-label={ariaLabel}
      className={cx(
        'inline-flex items-center gap-0.5 rounded-[8px] border border-line bg-sunken p-[3px]',
        className,
      )}
    >
      {options.map((opt) => {
        const active = opt.value === value;
        return (
          <button
            key={opt.value}
            role="tab"
            aria-selected={active}
            type="button"
            disabled={opt.disabled}
            onClick={() => onChange(opt.value)}
            className={cx(
              'select-none whitespace-nowrap rounded-[6px] font-medium transition-colors disabled:pointer-events-none disabled:opacity-40',
              size === 'sm' ? 'h-[22px] px-2.5 text-[11.5px]' : 'h-[24px] px-3 text-[12px]',
              active
                ? tone === 'accent'
                  ? 'bg-accent text-white shadow-[var(--shadow-seg)]'
                  : 'bg-raised text-ink shadow-[var(--shadow-seg)]'
                : 'text-ink3 hover:bg-hover hover:text-ink',
            )}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
