import type { ReactNode } from 'react';
import { cx } from '../lib/util';

export interface RowProps {
  id?: string;
  label: ReactNode;
  hint?: ReactNode;
  children: ReactNode;
  className?: string;
  flashing?: boolean;
}

/**
 * 设置行：左边标签（+ 一句说明），右边控件**贴齐行尾**，两者垂直居中。
 *
 * 以前每行还挂一枚作用域小标签（APP / 全局 / 本版本），但同一页里这些标签
 * 几乎全部相同——同样的字重复十几遍只会变成噪声。真正有区别的作用域写进
 * 该行的说明文字里，标签不再重复。
 *
 * 控件列用 `justify-end` 收在右边：设置列表的常规读法是「左边是什么、右边调什么」，
 * 控件悬在中间会让每一行的起点各不相同，扫视时反而在找对齐线。
 * 需要占满整列的控件（滑块、多行文本）自己带 `w-full`，不吃这条规则。
 *
 * 也不用 `items-start`：说明文字一长，控件就被顶到行首，看起来像没对齐。
 */
export function Row({ id, label, hint, children, className, flashing }: RowProps) {
  return (
    <div
      id={id}
      className={cx(
        'grid grid-cols-[200px_minmax(0,1fr)] items-center gap-6 border-b border-line px-4 py-3.5 last:border-b-0',
        flashing && 'row-flash',
        className,
      )}
    >
      <div className="min-w-0">
        <span className="text-[12.5px] font-medium text-ink">{label}</span>
        {hint ? <p className="mt-1 text-[11px] leading-[1.55] text-ink4">{hint}</p> : null}
      </div>
      <div className="flex min-w-0 items-center justify-end">{children}</div>
    </div>
  );
}
