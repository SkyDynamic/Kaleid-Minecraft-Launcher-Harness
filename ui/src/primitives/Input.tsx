import type { ComponentPropsWithRef } from 'react';
import { cx } from '../lib/util';

export type InputProps = ComponentPropsWithRef<'input'> & { invalid?: boolean };

export function Input({ invalid = false, className, ...rest }: InputProps) {
  return (
    <input
      {...rest}
      className={cx(
        'h-[30px] w-full rounded-[7px] border bg-field px-2.5 text-[12.5px] text-ink',
        'placeholder:text-ink4 focus:outline-none transition-colors',
        invalid
          ? 'border-red/70 ring-1 ring-red/40 focus:border-red'
          : 'border-line hover:border-line-strong focus:border-accent/70 focus:ring-1 focus:ring-accent/25',
        className,
      )}
    />
  );
}

export type TextareaProps = ComponentPropsWithRef<'textarea'> & { invalid?: boolean };

export function Textarea({ invalid = false, className, ...rest }: TextareaProps) {
  return (
    <textarea
      {...rest}
      className={cx(
        'w-full resize-y rounded-[7px] border bg-field px-2.5 py-2 font-mono text-[11.5px] leading-[1.7] text-ink2',
        'placeholder:text-ink4 focus:outline-none transition-colors',
        invalid
          ? 'border-red/70 focus:border-red'
          : 'border-line hover:border-line-strong focus:border-accent/70',
        className,
      )}
    />
  );
}
