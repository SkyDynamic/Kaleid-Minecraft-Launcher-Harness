import { cx } from '../lib/util';

export interface SwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  ariaLabel?: string;
}

export function Switch({ checked, onChange, disabled, ariaLabel }: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cx(
        'relative inline-flex h-[18px] w-[32px] shrink-0 items-center rounded-full border transition-colors',
        'disabled:pointer-events-none disabled:opacity-40',
        checked ? 'border-accent/60 bg-accent' : 'border-line-strong bg-layer3',
      )}
    >
      <span
        className={cx(
          'inline-block h-[12px] w-[12px] rounded-full border border-line-strong bg-white shadow-[0_1px_2px_rgba(0,0,0,.35)] transition-transform',
          checked ? 'translate-x-[16px]' : 'translate-x-[3px]',
        )}
      />
    </button>
  );
}
