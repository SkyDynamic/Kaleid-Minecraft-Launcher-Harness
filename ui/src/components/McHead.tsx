import { useEffect, useState, type CSSProperties } from 'react';
import { getBridge } from '../bridge';
import { SkinHead } from './SkinHead';

const skinCache = new Map<string, Promise<string | null>>();

function lookupSkin(name: string, uuid: string): Promise<string | null> {
  const key = (uuid || name).trim().toLowerCase();
  const cached = skinCache.get(key);
  if (cached) return cached;
  const request = getBridge()
    .invoke<string | null>('skin_resolve', {
      name: name || null,
      uuid: uuid || null,
      url: null,
    })
    .catch(() => null);
  skinCache.set(key, request);
  return request;
}

export interface McHeadProps {
  name: string;
  uuid?: string;
  /**
   * 皮肤贴图直链。有值就按 `avaHead3` 的正面裁切渲染；
   * 没有就按 `skinModel` 使用内置 Steve / Alex（同样走双层裁切）。
   */
  skinUrl?: string | null;
  /** `slim` → 亚历克斯，其它 → 史蒂夫。 */
  skinModel?: string | null;
  size?: number;
  className?: string;
  title?: string;
}

export function McHead({
  name,
  uuid = '',
  skinUrl = null,
  skinModel = null,
  size = 52,
  className,
  title,
}: McHeadProps) {
  const label = title ?? (name || uuid || '未登录');
  const [resolvedSkin, setResolvedSkin] = useState<string | null>(skinUrl ?? null);

  useEffect(() => {
    if (skinUrl) {
      setResolvedSkin(skinUrl);
      return;
    }
    if (!name && !uuid) {
      setResolvedSkin(null);
      return;
    }
    let alive = true;
    void lookupSkin(name, uuid).then((url) => {
      if (alive) setResolvedSkin(url);
    });
    return () => {
      alive = false;
    };
  }, [skinUrl, name, uuid]);

  const style: CSSProperties = {
    width: size,
    height: size,
    borderRadius: Math.max(6, Math.round(size * 0.19)),
    overflow: 'hidden',
    position: 'relative',
    flex: 'none',
    // 底色与描边走 token，浅色主题下头像框才不会糊成一团黑。
    background: 'var(--color-layer2)',
    boxShadow: 'inset 0 0 0 1px var(--color-line-strong), 0 1px 3px rgba(0,0,0,.18)',
  };

  return (
    <div className={className} style={style} title={label}>
      <SkinHead
        skinUrl={resolvedSkin}
        size={size}
        model={skinModel}
        alt={`${label} 的头像`}
        className="rounded-none"
      />
    </div>
  );
}
