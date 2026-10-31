import type { HTMLAttributes, ReactNode } from 'react';
import { cx } from '../lib/util';

export interface CardProps extends HTMLAttributes<HTMLDivElement> {
  children: ReactNode;
}

export function Card({ className, children, ...rest }: CardProps) {
  return (
    <div className={cx('card', className)} {...rest}>
      {children}
    </div>
  );
}

export interface LabelProps {
  children: ReactNode;
  className?: string;
}

/** Small uppercase dimmed card/section label. */
export function Label({ children, className }: LabelProps) {
  return (
    <div
      className={cx(
        'text-[10px] font-medium uppercase tracking-[0.09em] text-ink4 select-none',
        className,
      )}
    >
      {children}
    </div>
  );
}
