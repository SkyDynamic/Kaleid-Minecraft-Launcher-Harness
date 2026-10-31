import { useEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { cx } from '../lib/util';
import { uiScale } from '../store/prefs';

export interface PopoverProps {
  x: number;
  y: number;
  onClose: () => void;
  children: ReactNode;
  width?: number;
  maxHeight?: number;
}

export function Popover({ x, y, onClose, children, width = 176, maxHeight = 240 }: PopoverProps) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      if (!ref.current?.contains(event.target as Node)) onClose();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    // Defer so the click that opened the menu does not immediately close it.
    const timer = setTimeout(() => document.addEventListener('pointerdown', onPointerDown), 0);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [onClose]);

  // zoom 子树里 fixed 的 px 坐标会被再乘一遍缩放，所以先除回去；可用的边界也要
  // 按缩放后的视口尺寸算，否则字号调大后菜单会被推到窗口外面（看起来像"没弹出来"）。
  const scale = uiScale();
  const pad = 8 / scale;
  const left = Math.max(pad, Math.min(x / scale, window.innerWidth / scale - width - pad));
  const top = Math.max(pad, Math.min(y / scale, window.innerHeight / scale - maxHeight - pad));

  // 必须挂到 body 上：侧栏和底栏外面那层 `anim-fade-up` 的 `animation-fill-mode: both`
  // 会把 `translateY(0)` 一直留在元素上，于是它成了 fixed 的包含块又建了层叠上下文——
  // 菜单会跟着侧栏定位、而且被主区盖住（只剩侧栏宽度那一条可见）。
  return createPortal(
    <div
      ref={ref}
      role="menu"
      style={{ left, top, width, maxHeight }}
      className="fixed z-50 overflow-y-auto rounded-[8px] border border-line-strong bg-layer2 p-1 shadow-[var(--shadow-pop)]"
    >
      {children}
    </div>,
    document.body,
  );
}

export interface MenuItemProps {
  onClick: () => void;
  children: ReactNode;
  danger?: boolean;
  active?: boolean;
}

export function MenuItem({ onClick, children, danger = false, active = false }: MenuItemProps) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      className={cx(
        'flex h-[28px] w-full items-center justify-between gap-2 rounded-[5px] px-2 text-left text-[12px] transition-colors',
        danger ? 'text-red hover:bg-hover' : 'text-ink2 hover:bg-hover hover:text-ink',
        active && 'text-ink',
      )}
    >
      <span className="truncate">{children}</span>
      {active ? <span className="text-accent">✓</span> : null}
    </button>
  );
}

export function MenuLabel({ children }: { children: ReactNode }) {
  return (
    <div className="px-2 pb-1 pt-1.5 text-[10px] uppercase tracking-[0.08em] text-ink4">
      {children}
    </div>
  );
}

export function MenuDivider() {
  return <div className="my-1 h-px bg-line" />;
}
