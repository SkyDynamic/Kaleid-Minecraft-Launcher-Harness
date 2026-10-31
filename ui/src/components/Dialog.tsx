import {
  useEffect,
  useId,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { X } from 'lucide-react';
import { IconButton } from '../primitives';
import { cx } from '../lib/util';

/** 退场动画时长，必须与 theme.css 里的 `--dur-fast` 对齐。 */
const EXIT_MS = 130;

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: string;
  subtitle?: ReactNode;
  /** 面板宽度（px）；窄屏时自动收成 100%。 */
  width?: number;
  children: ReactNode;
  /** 固定在内容区下方、不参与滚动的底栏。 */
  footer?: ReactNode;
  bodyClassName?: string;
  /**
   * 内容区那个滚动容器的 ref。
   *
   * 「回到顶部」之类的按钮需要读 `scrollTop` / `scrollHeight`，而滚动条长在
   * Dialog 自己身上（不是调用方的子节点上），所以得把容器交出去。
   */
  bodyRef?: RefObject<HTMLDivElement | null>;
  /**
   * 浮在面板里、**不跟随内容滚动**的一层。
   *
   * 直接写进 `overflow-y-auto` 的内容里会被一起滚走，所以单独放在面板这一层
   * （`absolute inset-0`）。这一层默认 `pointer-events-none`，需要点击的按钮
   * 自己加 `pointer-events-auto`。
   */
  overlay?: ReactNode;
}

/** 当前打开的 Dialog 栈：Esc 只关闭最上面一层。 */
const dialogStack: string[] = [];

/**
 * 受控 Dialog。
 *
 * 刻意手写而不是引组件库：这里只需要"遮罩 + 面板 + Esc + 焦点 + 进出场动画"，
 * 引一个库要多几百 KB，还要为它再写一层样式覆盖。
 *
 * 退出时不能立刻卸载，否则 CSS 退场动画根本没机会播放——所以内部维护
 * `visible / closing` 两个状态，动画放完再真正移除节点。
 */
export function Dialog({
  open,
  onClose,
  title,
  subtitle,
  width = 760,
  children,
  footer,
  bodyClassName,
  bodyRef,
  overlay,
}: DialogProps) {
  const [visible, setVisible] = useState(open);
  const [closing, setClosing] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);
  const visibleRef = useRef(open);
  const dialogId = useId();

  useEffect(() => {
    visibleRef.current = visible;
  }, [visible]);

  // 打开：立刻挂载并播放入场；关闭：先播退场，再卸载。
  useEffect(() => {
    if (open) {
      setVisible(true);
      setClosing(false);
      return;
    }
    if (visibleRef.current) setClosing(true);
  }, [open]);

  useEffect(() => {
    if (!closing) return;
    const timer = setTimeout(() => {
      setVisible(false);
      setClosing(false);
    }, EXIT_MS);
    return () => clearTimeout(timer);
  }, [closing]);

  // 打开时登记到栈顶；只有栈顶 Dialog 响应 Esc，避免一次关掉两层。
  useEffect(() => {
    if (!open) return;
    dialogStack.push(dialogId);
    return () => {
      const index = dialogStack.lastIndexOf(dialogId);
      if (index >= 0) dialogStack.splice(index, 1);
    };
  }, [open, dialogId]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      if (dialogStack[dialogStack.length - 1] !== dialogId) return;
      event.preventDefault();
      onClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [open, onClose, dialogId]);

  // 打开时锁住背景滚动，并还原成打开前的值（不是硬写成 ''）。
  useEffect(() => {
    if (!visible) return;
    const previous = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.body.style.overflow = previous;
    };
  }, [visible]);

  // 打开后把焦点移进面板，键盘用户不会还停在触发按钮上。
  useEffect(() => {
    if (!open) return;
    const frame = requestAnimationFrame(() => panelRef.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, [open]);

  if (!visible) return null;

  return createPortal(
    <div className="fixed inset-0 z-[60] flex items-center justify-center p-6">
      <div
        aria-hidden="true"
        onClick={onClose}
        className={cx(
          'absolute inset-0 bg-scrim backdrop-blur-[2px]',
          closing ? 'anim-overlay-out' : 'anim-overlay-in',
        )}
      />
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        style={{
          width,
          maxWidth: '100%',
          // vh 是视口高度，不受整页缩放影响：不除以缩放比例的话，界面字号调到 20px
          // 时「86vh」实际画出来是窗口的 130%，弹窗上下都会顶出屏幕。
          maxHeight: 'calc(min(86vh, 760px) / var(--ui-zoom, 1))',
        }}
        className={cx(
          'relative flex flex-col overflow-hidden rounded-[12px]',
          'border border-line-strong bg-layer1 shadow-[var(--shadow-overlay)] outline-none',
          closing ? 'anim-scale-out' : 'anim-scale-in',
        )}
      >
        <header className="flex shrink-0 items-start justify-between gap-4 border-b border-line px-4 py-3">
          <div className="min-w-0">
            <h2 className="text-[14px] font-semibold leading-[19px] text-ink">{title}</h2>
            {subtitle ? (
              <p className="mt-1 text-[11px] leading-[15px] text-ink4">{subtitle}</p>
            ) : null}
          </div>
          <IconButton label="关闭" size={24} className="shrink-0" onClick={onClose}>
            <X size={14} />
          </IconButton>
        </header>
        <div ref={bodyRef} className={cx('min-h-0 flex-1 overflow-y-auto', bodyClassName)}>
          {children}
        </div>
        {overlay ? (
          <div className="pointer-events-none absolute inset-0 z-[1]">{overlay}</div>
        ) : null}
        {footer ? (
          <footer className="shrink-0 border-t border-line bg-layer1 px-4 py-3">{footer}</footer>
        ) : null}
      </div>
    </div>,
    document.body,
  );
}
