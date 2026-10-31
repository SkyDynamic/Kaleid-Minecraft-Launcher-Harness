import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, ChevronDown } from 'lucide-react';
import { cx } from '../lib/util';
import { uiScale } from '../store/prefs';

export interface SelectOption {
  value: string;
  label: string;
  /** 分组标题。同一组的选项连续排列，列表按出现顺序输出标题。 */
  group?: string;
}

export interface SelectProps {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  /** 读屏与 title 用的名字。 */
  ariaLabel: string;
  /** 当前值不在 options 里时显示它。 */
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  /** 下拉列表最大高度（px）。 */
  maxHeight?: number;
  /** 下拉列表宽度（px）；默认与按钮同宽。 */
  listWidth?: number;
}

interface Anchor {
  left: number;
  top: number;
  width: number;
}

/**
 * 手写下拉选择框。
 *
 * 为什么不用 `<select>`：原生控件在 WebView 里的弹层样式完全跟随系统，
 * 深色皮肤下会闪出一块白板；模组搜索卡的「来源 / 类型」又需要分组标题
 * （类型有 10 多个大类）。
 *
 * 弹层用 `position: fixed` 而不是绝对定位：Dialog 的内容区是
 * `overflow-y: auto`，绝对定位的下拉会被裁掉。
 */
export function Select({
  value,
  options,
  onChange,
  ariaLabel,
  placeholder,
  disabled = false,
  className,
  maxHeight = 280,
  listWidth,
}: SelectProps) {
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const open = anchor !== null;
  const selected = options.find((option) => option.value === value);
  const label = selected?.label ?? placeholder ?? value;

  const close = () => setAnchor(null);

  // 打开时按按钮位置摆弹层，空间不够就向上翻。
  useLayoutEffect(() => {
    if (!open) return;
    const rect = buttonRef.current?.getBoundingClientRect();
    if (!rect) return;
    // fixed 浮层的 px 坐标在 zoom 子树里会被再乘一遍缩放：先把视口坐标除回去，
    // 边界也按缩放后的视口算，弹层才会贴着按钮出现在窗口里。
    const scale = uiScale();
    const pad = 8 / scale;
    const viewW = window.innerWidth / scale;
    const viewH = window.innerHeight / scale;
    const width = listWidth ?? rect.width / scale;
    const height = Math.min(maxHeight, options.length * 26 + 12);
    const anchorTop = rect.top / scale;
    const below = viewH - rect.bottom / scale - pad;
    const up = below < height && anchorTop > below;
    setAnchor({
      left: Math.max(pad, Math.min(rect.left / scale, viewW - width - pad)),
      top: up ? Math.max(pad, anchorTop - height - 4 / scale) : rect.bottom / scale + 4 / scale,
      width,
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (listRef.current?.contains(target) || buttonRef.current?.contains(target)) return;
      close();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.stopPropagation();
        close();
        buttonRef.current?.focus();
      }
    };
    // 推迟一帧：打开菜单的那次点击不能立刻把它关掉。
    const timer = setTimeout(() => document.addEventListener('pointerdown', onPointerDown), 0);
    document.addEventListener('keydown', onKeyDown, true);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown, true);
    };
  }, [open]);

  const pick = (next: string) => {
    onChange(next);
    close();
    buttonRef.current?.focus();
  };

  let lastGroup: string | undefined;

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        role="combobox"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={ariaLabel}
        disabled={disabled}
        onClick={() => setAnchor(open ? null : { left: 0, top: 0, width: 0 })}
        className={cx(
          'inline-flex h-[26px] select-none items-center gap-1.5 rounded-[6px] border bg-field px-2',
          'text-[11.5px] text-ink2 transition-colors hover:border-line-strong hover:text-ink',
          'focus:outline-none focus-visible:border-accent/70 focus-visible:ring-1 focus-visible:ring-accent/25',
          'disabled:pointer-events-none disabled:opacity-40',
          open && 'border-accent/70 text-ink',
          className,
        )}
      >
        <span className="min-w-0 flex-1 truncate text-left">{label}</span>
        <ChevronDown
          size={11}
          className={cx('shrink-0 text-ink4 transition-transform', open && 'rotate-180')}
        />
      </button>

      {open && anchor
        ? createPortal(
            <div
              ref={listRef}
              role="listbox"
              aria-label={ariaLabel}
              style={{ left: anchor.left, top: anchor.top, width: anchor.width, maxHeight }}
              className="fixed z-[70] overflow-y-auto rounded-[8px] border border-line-strong bg-layer2 p-1 shadow-[var(--shadow-pop)]"
            >
              {options.map((option) => {
                const header = option.group && option.group !== lastGroup ? option.group : null;
                lastGroup = option.group ?? lastGroup;
                const active = option.value === value;
                return (
                  <div key={option.value}>
                    {header ? (
                      <div className="px-2 pb-1 pt-1.5 text-[10px] uppercase tracking-[0.08em] text-ink4">
                        {header}
                      </div>
                    ) : null}
                    <button
                      type="button"
                      role="option"
                      aria-selected={active}
                      onClick={() => pick(option.value)}
                      className={cx(
                        'flex h-[26px] w-full items-center justify-between gap-2 rounded-[5px] px-2 text-left text-[11.5px] transition-colors',
                        active ? 'bg-hover text-ink' : 'text-ink2 hover:bg-hover hover:text-ink',
                      )}
                    >
                      <span className="truncate">{option.label}</span>
                      {active ? <Check size={11} className="shrink-0 text-accent" /> : null}
                    </button>
                  </div>
                );
              })}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}
