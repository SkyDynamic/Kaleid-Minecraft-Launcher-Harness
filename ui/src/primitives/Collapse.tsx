/**
 * 可复用的展开/收起容器。
 *
 * 做法是 `grid-template-rows: 0fr → 1fr` + 内层 `min-height: 0; overflow: hidden`：
 * - 不需要测量高度，内容异步变高（图片、长文本）也不会"跳一下"；
 * - `height: auto` 无法过渡，`max-height` 猜一个值又会截断长内容，这里都不用。
 *
 * 收起动画结束后才卸载子树（否则内容会瞬间消失、看起来像"闪没了"）；
 * 系统「减少动态效果」与应用内「动效：关闭」由 theme.css 的全局闸门降成 0.01ms，
 * 于是过渡天然"立即完成"，这里不必再判一次偏好。
 */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { cx } from '../lib/util';

export interface CollapseProps {
  open: boolean;
  children: ReactNode;
  /** 额外类名，加在**外层**（尺寸/间距之类）。 */
  className?: string;
  /** 内层类名（内容自己的布局）。 */
  innerClassName?: string;
  /** 收起动画结束后是否卸载内容；默认 true。 */
  unmountOnClose?: boolean;
}

/** 与 `--dur-base`（170ms）对齐，仅用于"动画结束"的兜底定时器。 */
const FALLBACK_MS = 260;

export function Collapse({
  open,
  children,
  className,
  innerClassName,
  unmountOnClose = true,
}: CollapseProps) {
  // `mounted` 与 `open` 分开：关闭时先过渡再卸载。
  const [mounted, setMounted] = useState(open);
  const [expanded, setExpanded] = useState(open);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (timer.current) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    if (open) {
      // 先挂载（高度为 0）再下一帧打开，否则新内容没有起始状态、不会过渡。
      setMounted(true);
      const raf = requestAnimationFrame(() => setExpanded(true));
      return () => cancelAnimationFrame(raf);
    }
    setExpanded(false);
    if (!unmountOnClose) return;
    timer.current = setTimeout(() => setMounted(false), FALLBACK_MS);
    return () => {
      if (timer.current) {
        clearTimeout(timer.current);
        timer.current = null;
      }
    };
  }, [open, unmountOnClose]);

  if (!mounted && !open) return null;

  return (
    <div
      className={cx('grid transition-[grid-template-rows] duration-[var(--dur-base)]', className)}
      style={{
        gridTemplateRows: expanded ? '1fr' : '0fr',
        transitionTimingFunction: 'var(--ease-standard)',
      }}
      aria-hidden={!open}
      inert={!open ? true : undefined}
    >
      <div className={cx('min-h-0 overflow-hidden', innerClassName)}>{children}</div>
    </div>
  );
}
