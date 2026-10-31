/**
 * 模组图标。
 *
 * 图标是 base64 data URL，一个模组最多几百 KB：如果一次性把所有 mods 的图标都拉下来，
 * 打开模组页就会卡一下，而且**绝大多数图标用户根本不会看**。所以这里：
 * - 模块级缓存按「版本 + 文件名」记住已经取过的图标，切换页签不重复请求；
 * - 没有图标（`hasIcon === false`）或取失败都画一个统一的占位方块，
 *   而不是留空或报错——缺图标是常态，不是异常。
 */
import { useEffect, useState } from 'react';
import { getBridge } from '../bridge';
import type { ModIcon } from '../bridge/types';
import { cx } from '../lib/util';

/** `rootId|versionId|fileName` → data URL（`null` = 取过但没有）。 */
const iconCache = new Map<string, string | null>();

export function ModIconCell({
  rootId,
  versionId,
  fileName,
  hasIcon,
  dimmed,
}: {
  rootId: string;
  versionId: string;
  fileName: string;
  hasIcon: boolean;
  dimmed: boolean;
}) {
  const key = `${rootId}|${versionId}|${fileName}`;
  const [url, setUrl] = useState<string | null>(() => iconCache.get(key) ?? null);

  useEffect(() => {
    if (!hasIcon) return;
    const cached = iconCache.get(key);
    if (cached !== undefined) {
      setUrl(cached);
      return;
    }
    let alive = true;
    void (async () => {
      try {
        const icon = await getBridge().invoke<ModIcon>('content_mod_icon', {
          rootId,
          versionId,
          fileName,
        });
        iconCache.set(key, icon.dataUrl);
        if (alive) setUrl(icon.dataUrl);
      } catch {
        // 读不出图标（不是图片、太大、zip 损坏）都当"没有图标"
        iconCache.set(key, null);
        if (alive) setUrl(null);
      }
    })();
    return () => {
      alive = false;
    };
  }, [key, hasIcon, rootId, versionId, fileName]);

  return (
    <span
      className={cx(
        'flex h-[18px] w-[18px] shrink-0 items-center justify-center overflow-hidden rounded-[3px] border border-line',
        dimmed && 'opacity-45',
      )}
    >
      {url ? (
        <img src={url} alt="" draggable={false} className="h-full w-full object-cover" />
      ) : (
        <span
          aria-hidden="true"
          className="h-full w-full bg-[repeating-linear-gradient(45deg,var(--color-layer3)_0_3px,var(--color-layer2)_3px_6px)]"
        />
      )}
    </span>
  );
}

/** 让"重新读取"能拿到新图标：刷新列表时清掉缓存。 */
export function clearModIconCache() {
  iconCache.clear();
}
