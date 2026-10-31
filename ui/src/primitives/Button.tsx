import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { cx } from '../lib/util';

type Variant = 'primary' | 'secondary' | 'ghost' | 'subtle' | 'danger';
type Size = 'sm' | 'md' | 'lg';

const VARIANTS: Record<Variant, string> = {
  primary: 'bg-accent text-white hover:bg-accent-strong',
  secondary:
    'bg-layer2 text-ink2 border border-line hover:bg-layer3 hover:text-ink hover:border-line-strong',
  ghost: 'text-ink3 hover:bg-hover hover:text-ink',
  subtle: 'bg-hover text-ink2 hover:bg-hover-strong hover:text-ink',
  danger: 'text-red hover:bg-[color-mix(in_srgb,var(--color-red)_14%,transparent)]',
};

const SIZES: Record<Size, string> = {
  sm: 'h-[26px] px-2.5 text-[12px] rounded-[6px] gap-1.5',
  md: 'h-[30px] px-3 text-[12.5px] rounded-[7px] gap-1.5',
  lg: 'h-[34px] px-4 text-[13px] rounded-[8px] gap-2',
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  icon?: ReactNode;
}

export function Button({
  variant = 'subtle',
  size = 'md',
  icon,
  className,
  children,
  type = 'button',
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={cx(
        'inline-flex select-none items-center justify-center whitespace-nowrap font-medium transition-colors',
        'disabled:pointer-events-none disabled:opacity-40',
        VARIANTS[variant],
        SIZES[size],
        className,
      )}
      {...rest}
    >
      {icon}
      {children}
    </button>
  );
}

export interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  label: string;
  size?: number;
  active?: boolean;
}

export function IconButton({
  label,
  size = 26,
  active = false,
  className,
  children,
  type = 'button',
  ...rest
}: IconButtonProps) {
  return (
    <button
      type={type}
      title={label}
      aria-label={label}
      style={{ width: size, height: size }}
      className={cx(
        'inline-flex select-none items-center justify-center rounded-[6px] transition-colors',
        // 到边界时按钮要看得出来按不动：否则用户会以为点了没反应。
        'disabled:pointer-events-none disabled:opacity-40',
        active ? 'bg-hover text-ink' : 'text-ink4 hover:bg-hover hover:text-ink',
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
}
