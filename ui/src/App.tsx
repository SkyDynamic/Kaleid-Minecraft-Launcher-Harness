import {
  useEffect,
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import { Dock } from './shell/Dock';
import { RootChooserDialog } from './components/RootChooserDialog';
import { DemoDataBanner } from './components/DemoDataBanner';
import { DownloadQueueDialog } from './shell/DownloadQueueDialog';
import { InstallWizard } from './shell/InstallWizard';
import { MainPane } from './shell/MainPane';
import { ModDownloadDialog } from './shell/ModDownloadDialog';
import { NewsPanel } from './shell/NewsPanel';
import { FolderZone, PrimaryAction, Rail, SettingsFooter, VersionsZone } from './shell/Rail';
import { SettingsDialog } from './shell/SettingsDialog';
import { registerSlot, type SlotComponent } from './shell/slots';
import { LoginCard, Overview } from './pages/Overview';
import { GameSettings } from './pages/GameSettings';
import { Logs } from './pages/Logs';
import { Mods } from './pages/Mods';
import { ResourcePacks } from './pages/ResourcePacks';
import { Saves } from './pages/Saves';
import { useApp, usePluginEnabled } from './store/app';
import { hasActiveDownloads, useDownloads } from './store/downloads';
import { useModDownload } from './store/modDownload';
import { useUI } from './store/ui';
import {
  applyFont,
  applyFontSize,
  applyMotion,
  applyTheme,
  resolveTheme,
  uiScale,
  usePrefs,
} from './store/prefs';
import { cx } from './lib/util';

const PANELS: Record<string, SlotComponent> = {
  overview: Overview,
  mods: Mods,
  packs: ResourcePacks,
  saves: Saves,
  settings: GameSettings,
  logs: Logs,
};

function Splash() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="flex items-center gap-2.5">
        <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
        <span className="text-[12.5px] text-ink3">正在读取配置…</span>
      </div>
    </div>
  );
}

function SplitHandle({
  side,
  value,
  min,
  max,
  onResize,
}: {
  side: 'left' | 'right';
  value: number;
  min: number;
  max: number;
  onResize: (width: number) => void;
}) {
  const drag = useRef<{ x: number; value: number } | null>(null);

  const clamp = (next: number) => Math.min(max, Math.max(min, Math.round(next)));

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    drag.current = { x: event.clientX, value };
    event.currentTarget.setPointerCapture?.(event.pointerId);
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const onMove = (moveEvent: PointerEvent) => {
      if (!drag.current) return;
      // clientX 是缩放后的屏幕坐标，栏宽是未缩放的 CSS px：不除回去的话，
      // 字号放大后拖起来会「跑得比鼠标快」，拖不到想要的位置。
      const delta = (moveEvent.clientX - drag.current.x) / uiScale();
      onResize(clamp(drag.current.value + (side === 'left' ? delta : -delta)));
    };
    const finish = () => {
      drag.current = null;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', finish);
      window.removeEventListener('pointercancel', finish);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', finish);
    window.addEventListener('pointercancel', finish);
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    const direction = event.key === 'ArrowLeft' ? -1 : 1;
    onResize(clamp(value + (side === 'left' ? direction * 12 : -direction * 12)));
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={side === 'left' ? '调整左侧栏宽度' : '调整右侧栏宽度'}
      aria-valuenow={value}
      aria-valuemin={min}
      aria-valuemax={max}
      title="拖动调整宽度（方向键微调）"
      tabIndex={0}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
      className={cx(
        'group relative z-20 w-[5px] shrink-0 touch-none cursor-col-resize bg-transparent outline-none',
        'focus-visible:bg-accent/10',
      )}
    >
      <span className="pointer-events-none absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-transparent transition-colors group-hover:bg-accent/60 group-focus-visible:bg-accent/60" />
    </div>
  );
}

function Toast() {
  const toast = useUI((s) => s.toast);
  if (!toast) return null;
  return (
    <div className="pointer-events-none fixed bottom-4 left-1/2 z-50 -translate-x-1/2">
      <div className="anim-toast-in rounded-[7px] border border-line-strong bg-layer3 px-3 py-1.5 text-[11.5px] text-ink2 shadow-[0_8px_24px_rgba(0,0,0,.5)]">
        {toast}
      </div>
    </div>
  );
}

export function App() {
  const init = useApp((s) => s.init);
  const ready = useApp((s) => s.ready);
  const versionManagerEnabled = usePluginEnabled('version-manager');
  const modDownloadEnabled = usePluginEnabled('mod-download');
  const newsEnabled = usePluginEnabled('news');
  const refreshDownloads = useDownloads((s) => s.refresh);
  const motion = usePrefs((s) => s.motion);
  const theme = usePrefs((s) => s.theme);
  const font = usePrefs((s) => s.font);
  const fontSizePx = usePrefs((s) => s.fontSizePx);
  const railWidth = usePrefs((s) => s.railWidth);
  const dockWidth = usePrefs((s) => s.dockWidth);
  const setRailWidth = usePrefs((s) => s.setRailWidth);
  const setDockWidth = usePrefs((s) => s.setDockWidth);
  const railCollapsed = useUI((s) => s.railCollapsed);
  const dockVisible = newsEnabled || versionManagerEnabled;

  useEffect(() => {
    void init();
  }, [init]);

  // 动效偏好写到 <html data-motion>，样式层的闸门在 theme.css。
  useEffect(() => {
    applyMotion(usePrefs.getState());
  }, [motion]);

  // 主题：跟随系统时要订阅系统明暗变化，否则用户切了系统主题界面不会动。
  useEffect(() => {
    void applyFont(usePrefs.getState());
  }, [font]);

  // 界面字号：--ui-zoom 的比例在 theme.css 的 html 规则里生效。
  useEffect(() => {
    applyFontSize(usePrefs.getState());
  }, [fontSizePx]);

  useEffect(() => {
    const apply = () => applyTheme(resolveTheme(theme));
    apply();
    if (theme !== 'system' || typeof window.matchMedia !== 'function') return;
    const query = window.matchMedia('(prefers-color-scheme: light)');
    query.addEventListener('change', apply);
    return () => query.removeEventListener('change', apply);
  }, [theme]);

  // 下载队列轮询：有活动任务时 650ms，空闲时 2500ms。没有事件推送也能看到真实进度。
  useEffect(() => {
    if (!versionManagerEnabled) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // 上一帧各任务的状态：用来发现「刚跑完」的那一下。安装线程写完版本 JSON
    // 之后才应该重扫，向导提交时那次扫描通常还太早。
    const lastStatus = new Map<string, string>();
    const tick = async () => {
      if (stopped) return;
      await refreshDownloads();
      if (stopped) return;
      const snapshot = useDownloads.getState().snapshot;
      let finishedOne = false;
      for (const job of snapshot?.jobs ?? []) {
        const before = lastStatus.get(job.id);
        lastStatus.set(job.id, job.status);
        if (before === undefined) continue;
        const wasActive =
          before === 'queued' || before === 'resolving' || before === 'running';
        const nowDone =
          job.status === 'completed' ||
          job.status === 'failed' ||
          job.status === 'cancelled';
        if (wasActive && nowDone) finishedOne = true;
      }
      if (finishedOne) {
        void useApp.getState().refreshVersions();
      }
      const active = hasActiveDownloads(useDownloads.getState().snapshot);
      timer = setTimeout(() => void tick(), active ? 650 : 2500);
    };
    void tick();
    return () => {
      stopped = true;
      if (timer) clearTimeout(timer);
    };
  }, [refreshDownloads, versionManagerEnabled]);

  // 停用版本管理插件时，已经打开的向导/队列不能继续悬空。
  useEffect(() => {
    if (versionManagerEnabled) return;
    const ui = useUI.getState();
    ui.closeInstall();
    ui.setQueueOpen(false);
  }, [versionManagerEnabled]);

  // 同理：模组下载插件被停用时，对话框必须自己关掉（服务已经不在了）。
  useEffect(() => {
    if (modDownloadEnabled) return;
    useModDownload.getState().closeDialog();
  }, [modDownloadEnabled]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key === ',') {
        event.preventDefault();
        useUI.getState().openSettings('app');
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  // Register the full-featured slot implementations. When the version-manager
  // plugin is disabled the sidebar slots are unregistered and their read-only
  // fallbacks render instead.
  useEffect(() => {
    const unregister: Array<() => void> = [];

    for (const [id, Panel] of Object.entries(PANELS)) {
      unregister.push(registerSlot(`main.${id}`, Panel, 10, `panel-${id}`));
    }
    unregister.push(registerSlot('overview.login', LoginCard, 10, 'login-card'));
    unregister.push(registerSlot('sidebar.settings', SettingsFooter, 10, 'settings-footer'));

    if (newsEnabled) {
      unregister.push(registerSlot('dock.news', NewsPanel, 10, 'news-panel'));
    }
    if (versionManagerEnabled) {
      unregister.push(registerSlot('sidebar.header.action', PrimaryAction, 10, 'vm-action'));
      unregister.push(registerSlot('sidebar.roots', FolderZone, 10, 'vm-roots'));
      unregister.push(registerSlot('sidebar.versions', VersionsZone, 10, 'vm-versions'));
    }

    return () => {
      for (const off of unregister) off();
    };
  }, [versionManagerEnabled, newsEnabled]);

  const viewportWidth = typeof window === 'undefined' ? 1440 : window.innerWidth;
  const railMax = Math.max(200, Math.min(720, viewportWidth - dockWidth - 360 - 10));
  const dockMax = Math.max(200, Math.min(720, viewportWidth - railWidth - 360 - 10));

  return (
    <div className="flex h-full w-full overflow-hidden bg-bg text-ink">
      {ready ? (
        <>
          {/* 三个壳层错开 40ms 淡入：界面"活过来"要有先后感，但总时长不超过 220ms。 */}
          <div className="anim-fade-up flex min-h-0 shrink-0" style={{ animationDelay: '0ms' }}>
            <Rail />
          </div>
          {railCollapsed ? null : (
            <SplitHandle
              side="left"
              value={railWidth}
              min={200}
              max={railMax}
              onResize={setRailWidth}
            />
          )}
          <div
            className="anim-fade-up flex min-h-0 min-w-0 flex-1"
            style={{ animationDelay: '40ms' }}
          >
            <MainPane />
          </div>
          {dockVisible ? (
            <SplitHandle
              side="right"
              value={dockWidth}
              min={200}
              max={dockMax}
              onResize={setDockWidth}
            />
          ) : null}
          <div className="anim-fade-up flex min-h-0 shrink-0" style={{ animationDelay: '80ms' }}>
            <Dock />
          </div>
        </>
      ) : (
        <Splash />
      )}
      <SettingsDialog />
      <RootChooserDialog />
      <InstallWizard />
      <DownloadQueueDialog />
      <ModDownloadDialog />
      <Toast />
      <DemoDataBanner />
    </div>
  );
}
