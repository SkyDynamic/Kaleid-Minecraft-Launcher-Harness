import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { cx } from '../lib/util';

export interface TextLinkProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  children: ReactNode;
  tone?: 'accent' | 'muted';
}

export function TextLink({
  children,
  className,
  tone = 'accent',
  type = 'button',
  ...rest
}: TextLinkProps) {
  return (
    <button
      type={type}
      className={cx(
        'inline-flex select-none items-center gap-1 rounded-[4px] text-[11.5px] transition-colors disabled:opacity-40',
        tone === 'accent' ? 'text-accent hover:text-accent-hover' : 'text-ink3 hover:text-ink',
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
}
