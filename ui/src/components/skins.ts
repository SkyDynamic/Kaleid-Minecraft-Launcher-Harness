/**
 * 原版默认皮肤的 8×8 前脸材质。
 *
 * 为什么把像素写死在这里，而不是去网上取皮肤：未登录 / 离线账号根本没有可取的
 * 皮肤，而"占位图"必须长得像个人才有意义——官方默认皮肤（史蒂夫的 classic、
 * 亚历克斯的 slim）就是这两个角色本身。
 *
 * 坐标系：Minecraft 皮肤贴图 64×64，前脸落在 (8,8) 起的 8×8 区域。这里存的
 * 就是那 8×8 的颜色索引，`McHead` 再把它画成像素方块。
 */

export type FacePalette = Record<string, string>;

export type FaceRow = string;

export interface FaceMaterial {
  palette: FacePalette;
  /** 8 行 × 8 列，每字符对应 palette 里的一个颜色。 */
  rows: FaceRow[];
}

/** 史蒂夫（classic）：深棕短发、偏暖肤色。 */
export const STEVE: FaceMaterial = {
  palette: {
    H: '#4a382a',
    h: '#3a2b1f',
    s: '#b78b68',
    S: '#a67856',
    w: '#f2f2f2',
    e: '#2e2e38',
    n: '#94664a',
    m: '#6b4a32',
    b: '#7c5340',
  },
  rows: [
    'HHhhhhHH',
    'HhhhhhhH',
    'HssssssH',
    'HwessweH',
    'HssssssH',
    'ssSnnSss',
    'ssmmmmss',
    'sssbbsss',
  ],
};

/** 亚历克斯（slim）：橙棕长发、偏白肤色、绿眼。 */
export const ALEX: FaceMaterial = {
  palette: {
    H: '#c0663c',
    h: '#a8552f',
    s: '#d9b49a',
    S: '#c69e82',
    w: '#f2f2f2',
    e: '#3f7a3f',
    n: '#b98d72',
    m: '#8a5b47',
    b: '#9c6b54',
  },
  rows: [
    'HHhhhhHH',
    'HhhhhhhH',
    'HhsssshH',
    'hwessweh',
    'hssssssh',
    'ssSnnSss',
    'ssmmmmss',
    'sssbbsss',
  ],
};

export interface FaceCell {
  x: number;
  y: number;
  fill: string;
}

/** 把行字符串展开成方块列表；未知字符跳过（少一个像素好过整块渲染失败）。 */
export function faceCells(material: FaceMaterial): FaceCell[] {
  const cells: FaceCell[] = [];
  material.rows.forEach((row, y) => {
    for (let x = 0; x < row.length; x += 1) {
      const fill = material.palette[row[x]!];
      if (!fill) continue;
      cells.push({ x, y, fill });
    }
  });
  return cells;
}

/** `slim` 用亚历克斯，其余（含未登录）一律史蒂夫。 */
export function materialFor(skinModel?: string | null): FaceMaterial {
  return (skinModel ?? '').toLowerCase().includes('slim') ? ALEX : STEVE;
}
