import { useEffect, useRef, useState } from 'react';
import {
  DEFAULT_ALEX_SKIN,
  DEFAULT_STEVE_SKIN,
  OFFICIAL_STEVE_TEXTURE,
} from '../lib/assets';
import { cx } from '../lib/util';

export interface SkinHeadProps {
  /** 皮肤贴图直链；为空时按 model 使用内置 Steve/Alex。 */
  skinUrl?: string | null;
  size: number;
  /** classic / slim；slim 在没有皮肤直链时使用内置 Alex 兜底。 */
  model?: string | null;
  className?: string;
  alt?: string;
}

function upgradeMinecraftTexture(url: string): string {
  return url.startsWith('http://textures.minecraft.net/')
    ? `https://${url.slice('http://'.length)}`
    : url;
}

function isSlim(model: string | null | undefined): boolean {
  return (model ?? '').toLowerCase().includes('slim');
}

function defaultSkin(model: string | null | undefined): string {
  return isSlim(model) ? DEFAULT_ALEX_SKIN : upgradeMinecraftTexture(OFFICIAL_STEVE_TEXTURE);
}

/** 网络/贴图失败时的本地兜底，必须是内置资源，不能再指回远端。 */
function errorFallback(model: string | null | undefined): string {
  return isSlim(model) ? DEFAULT_ALEX_SKIN : DEFAULT_STEVE_SKIN;
}

function normalizeSkinUrl(url: string | null | undefined, model?: string | null): string {
  const trimmed = url?.trim() ?? '';
  if (!trimmed) return defaultSkin(model);
  return upgradeMinecraftTexture(trimmed);
}

/**
 * 按 mcskins.top `avaHead3` 的正面头部裁切方式渲染。
 *
 * `avaHead3` 是把 3D 头模型转到正前方（rotation.x = rotation.y = 0）后截头。
 * 这里用 CSS 等价地裁皮肤贴图：
 * - 底面：皮肤 (8,8) 的 8×8 脸；
 * - 第二层：皮肤 (40,8) 的 8×8 帽子/外层。
 *
 * 贴图按 `size * 8` 显示，再平移一个/五个头宽。旧版 64×32 皮肤没有外层，
 * 用 `naturalHeight >= naturalWidth` 判断；64×64 才叠加第二层。
 */
export function SkinHead({ skinUrl, size, model, className, alt }: SkinHeadProps) {
  const [src, setSrc] = useState(() => normalizeSkinUrl(skinUrl, model));
  const [loaded, setLoaded] = useState(false);
  const [hasOverlay, setHasOverlay] = useState(false);
  const failedSrc = useRef<string | null>(null);

  useEffect(() => {
    setSrc(normalizeSkinUrl(skinUrl, model));
    setLoaded(false);
    setHasOverlay(false);
    failedSrc.current = null;
  }, [skinUrl, model]);

  const imageWidth = size * 8;

  return (
    <span
      role="img"
      aria-label={alt ?? `${model ?? 'classic'} 皮肤头像`}
      className={cx(
        'relative block shrink-0 overflow-hidden rounded-[3px] bg-[#2b2b2d]',
        className,
      )}
      style={{ width: size, height: size }}
    >
      <img
        src={src}
        alt=""
        draggable={false}
        onLoad={(event) => {
          const image = event.currentTarget;
          setLoaded(true);
          setHasOverlay(image.naturalHeight >= image.naturalWidth);
        }}
        onError={() => {
          const fallback = errorFallback(model);
          if (src !== fallback && failedSrc.current !== src) {
            failedSrc.current = src;
            setSrc(fallback);
            setLoaded(false);
            setHasOverlay(false);
          }
        }}
        className="pointer-events-none absolute max-w-none select-none transition-opacity duration-200 [image-rendering:pixelated]"
        style={{
          width: imageWidth,
          height: 'auto',
          left: -size,
          top: -size,
          opacity: loaded ? 1 : 0,
        }}
      />
      {hasOverlay ? (
        <img
          src={src}
          alt=""
          aria-hidden="true"
          draggable={false}
          className="pointer-events-none absolute max-w-none select-none transition-opacity duration-200 [image-rendering:pixelated]"
          style={{
            width: imageWidth,
            height: 'auto',
            left: -5 * size,
            top: -size,
            opacity: loaded ? 1 : 0,
          }}
        />
      ) : null}
    </span>
  );
}
