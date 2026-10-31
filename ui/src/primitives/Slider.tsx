import type { CSSProperties } from 'react';
import { cx } from '../lib/util';

export interface SliderProps {
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (value: number) => void;
  className?: string;
  ariaLabel?: string;
  disabled?: boolean;
}

export function Slider({
  value,
  min,
  max,
  step,
  onChange,
  className,
  ariaLabel,
  disabled,
}: SliderProps) {
  const fill = max === min ? 0 : ((value - min) / (max - min)) * 100;
  return (
    <input
      type="range"
      className={cx('slider', className)}
      style={{ '--fill': `${fill}%` } as CSSProperties}
      min={min}
      max={max}
      step={step}
      value={value}
      disabled={disabled}
      aria-label={ariaLabel}
      onChange={(e) => onChange(Number(e.target.value))}
    />
  );
}
