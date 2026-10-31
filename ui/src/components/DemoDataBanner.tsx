/**
 * 「演示数据」提示条。
 *
 * 只出现在浏览器预览（没有宿主桥、开发构建）里。目的只有一个：让任何人一眼看出
 * 屏幕上的模组/存档/账号都**不是真的**，免得拿着假数据当结论。
 *
 * 真机（Tauri）里 `isMockBridge()` 恒为 false，这条永远不会渲染。
 */
import { FlaskConical } from 'lucide-react';
import { isMockBridge } from '../bridge';

export function DemoDataBanner() {
  if (!isMockBridge()) return null;
  return (
    <div className="pointer-events-none fixed bottom-2 left-1/2 z-[60] -translate-x-1/2">
      <div className="pointer-events-auto flex items-center gap-1.5 rounded-full border border-amber/40 bg-amber/15 px-2.5 py-1 text-[10.5px] leading-[15px] text-amber shadow-[var(--shadow-overlay)]">
        <FlaskConical size={11} className="shrink-0" />
        浏览器演示模式：页面上的数据都是示例，不写入磁盘
      </div>
    </div>
  );
}
