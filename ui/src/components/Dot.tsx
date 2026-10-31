import { cx } from '../lib/util';

export type DotTone = 'green' | 'amber' | 'gray' | 'red' | 'accent';

const TONES: Record<DotTone, string> = {
  green: 'bg-green',
  amber: 'bg-amber',
  gray: 'bg-ink4',
  red: 'bg-red',
  accent: 'bg-accent',
};

export function Dot({ tone = 'gray', size = 6, className }: { tone?: DotTone; size?: number; className?: string }) {
  return (
    <span
      className={cx('inline-block shrink-0 rounded-full', TONES[tone], className)}
      style={{ width: size, height: size }}
    />
  );
}
