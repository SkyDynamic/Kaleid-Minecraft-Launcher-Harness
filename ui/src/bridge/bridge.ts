import { mockBridge } from './mock';

export interface KaleidBridge {
  invoke<T>(cmd: string, args?: unknown): Promise<T>;
  listen(event: string, cb: (payload: unknown) => void): Promise<() => void>;
  /**
   * 用系统浏览器打开一个外部链接。
   *
   * 可选：老外壳没有这个方法，界面必须自己兜底（否则新闻点了没反应，
   * 用户会以为新闻是假的）。宿主侧只放行 http/https。
   */
  openExternal?(url: string): Promise<void>;
  /**
   * 切换外壳自己的标题栏明暗。
   *
   * 可选：浏览器里没有外壳，纯 CSS 变量已经够换肤色了；但在 Tauri 里
   * 不改这一项，浅色界面会顶着一条黑色标题栏。
   */
  setTheme?(theme: 'dark' | 'light'): Promise<void>;
  /** 原生文件夹选择器；浏览器 mock 下返回 `null`。 */
  pickFolder?(): Promise<string | null>;
  /**
   * 原生多选文件对话框（导入模组/资源包）。
   *
   * `extensions` 为空表示不限类型；用户取消返回空数组。
   */
  pickFiles?(extensions?: string[]): Promise<string[]>;
  /**
   * 订阅系统级文件拖放。
   *
   * Windows 上 Tauri 接管了 WebView2 的拖放，HTML5 `drop` 事件拿不到路径，
   * 所以路径由外壳转发过来。返回取消订阅的函数。
   */
  onFileDrop?(cb: (paths: string[]) => void): () => void;
  /** 用资源管理器打开一个目录。 */
  openDir?(path: string): Promise<void>;
  /**
   * 启动游戏后的窗口动作。
   *
   * 只有外壳能改窗口（浏览器里没有"最小化到托盘"这回事），所以是可选的：
   * 界面在浏览器预览时只会看到提示，不会有副作用。
   * `restore` 是回头路——隐藏到托盘后游戏退出，窗口得自己回来。
   */
  applyAfterLaunch?(action: 'minimize' | 'hide_to_tray' | 'exit' | 'restore'): Promise<void>;
  /** 原生字体文件选择器；返回绝对路径。 */
  pickFont?(): Promise<string | null>;
  /** 系统已安装字体族。 */
  listFonts?(): Promise<string[]>;
  /** 读取字体文件为 base64。 */
  readFont?(path: string): Promise<{
    path: string;
    name: string;
    fileName: string | null;
    extension: string;
    size: number;
    dataBase64: string;
  }>;
}

declare global {
  interface Window {
    /** Injected by the Tauri host (preload script). Absent in plain browsers. */
    __KALEID__?: KaleidBridge;
  }
}

let cached: KaleidBridge | null = null;

/** 浏览器里 `pnpm dev` 的演示数据只在开发构建里存在。 */
const DEV = import.meta.env.DEV;

/**
 * 宿主桥缺失时的"空桥"。
 *
 * 不能退化成 mock：真机上"拿不到内核"意味着启动器坏了，这时候给一份假数据
 * 比报错更糟——用户会以为模组/存档/账号是真的，然后对着假列表操作。
 * 所以每个调用都直接失败，并说清楚发生了什么。
 */
const BRIDGE_MISSING_MESSAGE =
  '没有连上启动器内核（宿主桥缺失）。这不是演示模式，所以不会显示示例数据：请重启启动器；仍然不行就重新安装。';

function deadBridge(): KaleidBridge {
  const fail = () => Promise.reject(new Error(BRIDGE_MISSING_MESSAGE));
  return {
    invoke: fail,
    listen: fail,
  };
}

/**
 * Returns the host bridge when running inside Tauri, otherwise the in-memory
 * mock so `pnpm dev` works standalone in a browser.
 */
export function getBridge(): KaleidBridge {
  if (cached) return cached;
  const host = typeof window !== 'undefined' ? window.__KALEID__ : undefined;
  if (host) {
    cached = host;
    return cached;
  }
  // 生产构建里没有宿主桥 = 出故障了，绝不拿 mock 顶上。
  cached = DEV ? mockBridge : deadBridge();
  return cached;
}

/**
 * 当前是不是"浏览器演示数据"。
 *
 * 界面上有一块常驻提示条用它：演示数据和真实数据混在一起是最容易骗人的，
 * 所以宁可丑一点也要标出来。
 */
export function isMockBridge(): boolean {
  if (typeof window !== 'undefined' && window.__KALEID__) return false;
  return DEV;
}

/**
 * 用资源管理器打开目录；外壳不支持时返回 `false`（调用方自己提示）。
 */
export async function openDirInShell(path: string): Promise<boolean> {
  const bridge = getBridge();
  if (!bridge.openDir) return false;
  try {
    await bridge.openDir(path);
    return true;
  } catch {
    return false;
  }
}

/**
 * 打开外部链接：优先交给外壳（真正的系统浏览器），否则退回 `window.open`。
 *
 * 不走 `<a href>` 直接跳转：WebView 里那样会把整个界面导航走，
 * 用户回不来。必须先 preventDefault。
 */
export async function openExternal(url: string): Promise<boolean> {
  const trimmed = url.trim();
  if (!/^https?:\/\//i.test(trimmed)) return false;

  const bridge = getBridge();
  if (bridge.openExternal) {
    try {
      await bridge.openExternal(trimmed);
      return true;
    } catch {
      // 外壳拒绝了（例如协议不在白名单）：下面还有一次浏览器兜底。
    }
  }
  if (typeof window !== 'undefined') {
    window.open(trimmed, '_blank', 'noopener,noreferrer');
    return true;
  }
  return false;
}
