import grassBlock from '../assets/versions/grass-block.png';
import commandBlock from '../assets/versions/command-block.png';
import cobblestone from '../assets/versions/cobblestone.png';
import goldBlock from '../assets/versions/gold-block.png';
import fabricIcon from '../assets/loaders/fabric.png';
import forgeIcon from '../assets/loaders/forge.png';
import neoforgeIcon from '../assets/loaders/neoforge.png';
import optifineIcon from '../assets/loaders/optifine.svg';
import steveSkin from '../assets/skins/steve-classic-64x64.png';
import alexSkin from '../assets/skins/alex-slim-64x64.png';

export const VERSION_ICONS = {
  release: grassBlock,
  snapshot: commandBlock,
  ancient: cobblestone,
  april_fools: goldBlock,
} as const;

export const LOADER_ICONS = {
  vanilla: grassBlock,
  fabric: fabricIcon,
  forge: forgeIcon,
  neoforge: neoforgeIcon,
  /** 自绘的镜片图标（官方没有可再分发的商标图）。 */
  optifine: optifineIcon,
} as const;

/** 内置的 Mojang 官方经典 Steve 皮肤；远端官方纹理加载失败时兜底。 */
export const DEFAULT_STEVE_SKIN = steveSkin;

/** 内置的 Mojang 官方 slim（Alex）默认皮肤；slim 账号无皮肤直链时用它。 */
export const DEFAULT_ALEX_SKIN = alexSkin;

/**
 * 官方经典 Steve 皮肤直链（Mojang 的原始地址是 HTTP）。
 * `SkinHead` 会统一升到 HTTPS，避免 WebView 混合内容拦截。
 */
export const OFFICIAL_STEVE_TEXTURE =
  'http://textures.minecraft.net/texture/963438953a782da769480b0a49125a92e259207700cb8e3e1aac38e471e20308';
