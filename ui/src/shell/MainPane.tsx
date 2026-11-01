import { useEffect, useRef, useState } from 'react';
import { Loader2, Play } from 'lucide-react';
import { getBridge } from '../bridge';
import type { CrashReport, LaunchBlock, LaunchCheck, LaunchState, VersionSummary } from '../bridge/types';
import { Button, Tabs, type TabItem } from '../primitives';
import { CrashDialog } from '../components/CrashDialog';
import { LaunchBlockedDialog } from '../components/LaunchBlockedDialog';
import { LaunchOverlay } from '../components/LaunchOverlay';
import { Slot } from './slots';
import { useApp, usePluginEnabled } from '../store/app';
import { useContent, useContentSync } from '../store/content';
import { launchIsBusy, useLaunch } from '../store/launch';
import { useSettings } from '../store/settings';
import { useUI } from '../store/ui';

function loaderLabel(version: VersionSummary | null): string | null {
  if (!version) return null;
  const base =
    version.loader === 'vanilla'
      ? '原版'
      : version.loader === 'fabric'
        ? 'Fabric'
        : version.loader === 'forge'
          ? 'Forge'
          : version.loader === 'neoforge'
            ? 'NeoForge'
            : version.loader === 'optifine'
              ? 'OptiFine'
              : '未识别';
  return version.loaderVersion ? `${base} ${version.loaderVersion}` : base;
}

function EmptyPanel() {
  return <div className="px-5 py-8 text-[12px] text-ink4">此面板暂不可用</div>;
}

export function MainPane() {
  const version = useApp((s) => s.versions.find((v) => v.id === s.activeVersionId) ?? null);
  const root = useApp((s) => s.roots.find((r) => r.id === s.activeRootId) ?? null);
  const globalSettings = useSettings((s) => s.global);
  const tab = useUI((s) => s.tab);
  const setTab = useUI((s) => s.setTab);
  const showToast = useUI((s) => s.showToast);

  // 页签上的数字也要跟磁盘一致，所以这里同样订阅内容缓存
  const contentEnabled = usePluginEnabled('game-content');
  useContentSync(contentEnabled);
  const mods = useContent((s) => s.mods);
  const packs = useContent((s) => s.packs);
  const saves = useContent((s) => s.saves);
  const logs = useContent((s) => s.logs);
  const dirs = useContent((s) => s.dirs);

  const chip = loaderLabel(version);
  const gameDir = dirs?.gameDir ?? (!root ? '未选择文件夹' : root.path);

  // ---- 真实启动 ----
  const [launching, setLaunching] = useState(false);
  /** 正在用遮罩展示的会话（PCL2 的启动进度窗口）。 */
  const [overlayFor, setOverlayFor] = useState<number | null>(null);
  const [crash, setCrash] = useState<CrashReport | null>(null);
  // 启动前检查没过：把原因弹出来，并且**不启动**。
  const [blocked, setBlocked] = useState<LaunchBlock[] | null>(null);
  // 启动完成/游戏退出后要恢复窗口（「隐藏至托盘」那条路的回头路）
  const hiddenRef = useRef(false);
  // 已经处理过"退出"的会话，避免轮询重复弹窗
  const reportedRef = useRef<Set<number>>(new Set());

  // 会话状态统一由 store 轮询（概览页的实例面板用的是同一份数据）。
  const sessions = useLaunch((s) => s.sessions);
  useEffect(() => useLaunch.getState().watch(), []);

  const isBusy = (s: LaunchState) =>
    s.phase === 'preparing' || s.phase === 'launching' || s.phase === 'running';
  /** 当前挑中的这个版本自己的会话（多开时别的版本在跑不能算在它头上）。 */
  const mine = sessions.filter((s) => s.versionId === version?.id);
  const running = mine.some(isBusy);
  /** 全局还有几个在跑：按钮上要如实说"再点就是多开一个"。 */
  const runningCount = sessions.filter(isBusy).length;
  /** 遮罩要展示的会话：还在准备/等窗口才有进度可看。 */
  const overlayState = sessions.find((s) => s.sessionId === overlayFor) ?? null;

  /** 启动完毕后的窗口动作：最小化 / 隐藏到托盘 / 退出 / 保持不变。 */
  const applyAfterLaunch = (state: LaunchState) => {
    const action = globalSettings?.launchAfterAction ?? 'keep';
    if (state.phase !== 'running' || action === 'keep') return;
    hiddenRef.current = action === 'hide_to_tray' || action === 'minimize';
    void getBridge().applyAfterLaunch?.(action);
  };

  /**
   * 会话一结束就收尾：崩溃弹报告、正常退出提示、隐藏到托盘的窗口回来。
   *
   * 数据来自 `useLaunch` 的轮询（它会一直轮下去），这里只对"新出现的终态"做一次
   * 处理——`reportedRef` 保证同一个会话不会弹两次。
   */
  useEffect(() => {
    for (const state of sessions) {
      if (state.phase !== 'exited' && state.phase !== 'failed' && state.phase !== 'cancelled') {
        continue;
      }
      if (reportedRef.current.has(state.sessionId)) continue;
      reportedRef.current.add(state.sessionId);

      // 先看"崩没崩"：进过游戏再崩，phase 也是 failed（要让崩溃弹窗接住），
      // 但那不是"启动失败"，别把覆盖层又弹出来吓人。
      if (state.crashed) {
        if (overlayFor === state.sessionId) setOverlayFor(null);
        setLaunching(false);
        void (async () => {
          try {
            const report = await getBridge().invoke<CrashReport>('crash_report', {
              sessionId: state.sessionId,
            });
            setCrash(report);
          } catch {
            showToast(`${state.versionId} 崩溃了，但读不到崩溃报告`);
          }
        })();
      } else if (state.phase === 'failed') {
        // 窗口都没出来就失败了：遮罩留在原地显示原因，等用户点「知道了」。
        setOverlayFor(state.sessionId);
        setLaunching(false);
      } else if (state.phase === 'cancelled') {
        if (overlayFor === state.sessionId) setOverlayFor(null);
        setLaunching(false);
        showToast(`已取消启动 ${state.versionId}`);
      } else {
        if (overlayFor === state.sessionId) setOverlayFor(null);
        const who = sessions.filter(isBusy).length > 0 ? `${state.versionId} ` : '';
        showToast(`${who}游戏已退出${state.exitCode ? `（退出码 ${state.exitCode}）` : ''}`);
      }
      if (hiddenRef.current) {
        hiddenRef.current = false;
        void getBridge().applyAfterLaunch?.('restore');
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessions]);

  /**
   * 遮罩的收场：立刻启动返回后一直挂着，直到这个会话进了游戏（Running）为止。
   *
   * 之所以不在 `Running` 时就立刻关掉：窗口刚出现的那一瞬游戏还在加载资源，
   * 立刻抽走遮罩会让人觉得"闪了一下"。这里让它留到 Running 再滑走，
   * 顺便把「启动后动作」补上。
   */
  useEffect(() => {
    if (overlayFor === null) return;
    const state = sessions.find((s) => s.sessionId === overlayFor);
    if (!state) return;
    if (state.phase === 'running') {
      setOverlayFor(null);
      setLaunching(false);
      applyAfterLaunch(state);
    } else if (state.phase === 'launching') {
      // 已经在等窗口了：按钮不必再转，进度条自己会说话
      setLaunching(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessions, overlayFor]);

  // 页面切回来时把"已经结束的会话"记为已处理，别又弹一次
  useEffect(() => {
    for (const state of useLaunch.getState().sessions) {
      if (!launchIsBusy(state)) reportedRef.current.add(state.sessionId);
    }
  }, []);

  const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

  const tabItems: TabItem[] = [
    { id: 'overview', label: '概览' },
    { id: 'mods', label: '模组', count: mods.length },
    { id: 'packs', label: '资源包', count: packs.length },
    { id: 'saves', label: '存档', count: saves.length },
    { id: 'settings', label: '游戏设置' },
    { id: 'logs', label: '日志', count: logs.length },
  ];

  /**
   * 启动：先 `launch_check` 再 `launch_start`。
   *
   * 登录失效/缺 Java/版本不完整都必须**在启动之前**拦住并把原因弹给用户，
   * 而不是开一个必然失败的游戏再看崩溃报告。
   */
  const launch = async () => {
    if (!version || !root) return;
    let check: LaunchCheck | null = null;
    try {
      check = await getBridge().invoke<LaunchCheck>('launch_check', {
        rootId: root.id,
        versionId: version.id,
      });
    } catch (e) {
      setBlocked([{ kind: 'version', message: errorText(e) }]);
      return;
    }
    if (check && !check.ok) {
      setBlocked(
        check.blocks.length > 0
          ? check.blocks
          : [{ kind: 'version', message: '启动前检查没有通过，但没有给出原因' }],
      );
      return;
    }
    setLaunching(true);
    try {
      const state = await getBridge().invoke<LaunchState>('launch_start', {
        rootId: root.id,
        versionId: version.id,
      });
      if (state.notes?.length) showToast(state.notes[0]);
      reportedRef.current.delete(state.sessionId);
      // `launch_start` 现在立刻返回「准备中」，真正的工作在后台线程里跑：
      // 遮罩挂上去，进度靠 store 轮询推进。
      setOverlayFor(state.sessionId);
      void useLaunch.getState().refresh();
    } catch (e) {
      setLaunching(false);
      const message = errorText(e);
      const kind = /登录|令牌|账号/.test(message) ? 'auth' : 'version';
      setBlocked([{ kind, message }]);
    }
  };

  /** 遮罩上的「取消启动」：后端只在准备阶段接受取消，晚了会给出人话。 */
  const cancelLaunch = async (sessionId: number) => {
    try {
      await useLaunch.getState().cancel(sessionId);
    } catch (e) {
      showToast(errorText(e));
    }
  };

  return (
    <main className="flex min-w-0 flex-1 flex-col bg-bg">
      <header className="flex shrink-0 items-start justify-between gap-6 px-5 pb-3 pt-4">
        <div className="min-w-0">
          <div className="flex min-w-0 items-center gap-2">
            <h1 className="truncate text-[15px] font-semibold leading-[20px] text-ink">
              {version ? version.id : '未选择版本'}
            </h1>
            {chip ? (
              <span className="shrink-0 rounded-[4px] border border-line bg-layer2 px-1.5 py-[1px] text-[10px] leading-[15px] text-ink3">
                {chip}
              </span>
            ) : null}
          </div>
          <div className="mt-1 truncate font-mono text-[10.5px] leading-[14px] text-ink4">
            {gameDir}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-2 pt-[1px]">
          {running ? (
            <span className="flex items-center gap-1.5 text-[10.5px] text-ink3">
              <span className="h-[6px] w-[6px] rounded-full bg-accent" />
              {launching ? '启动中…' : '运行中'}
              {runningCount > 1 ? (
                <span className="font-mono text-[10px] text-ink4">{runningCount} 个进程</span>
              ) : null}
            </span>
          ) : null}
          {/* 没有「结束游戏」按钮：直接关游戏窗口就行。命令 `launch_stop` 还在，
              CLI 与将来的托盘菜单仍然可以用它。 */}
          <Button
            variant="primary"
            size="md"
            icon={
              launching ? (
                <Loader2 size={12} className="animate-spin" />
              ) : (
                <Play size={12} className="fill-current" />
              )
            }
            disabled={!version || launching}
            onClick={() => void launch()}
            title={running ? '再点一次就是多开一个' : undefined}
          >
            {launching ? '启动中…' : running ? '再启动一个' : '启动游戏'}
          </Button>
        </div>
      </header>

      <CrashDialog report={crash} onClose={() => setCrash(null)} />
      <LaunchBlockedDialog blocks={blocked} onClose={() => setBlocked(null)} />
      <LaunchOverlay
        state={overlayState}
        onCancel={(id) => void cancelLaunch(id)}
        onClose={() => setOverlayFor(null)}
      />

      <Tabs items={tabItems} value={tab} onChange={(id) => setTab(id as typeof tab)} />

      {/* 这一层是"页面区"：普通页面的长内容由它滚；
          `fill` 的页面（如日志）靠它拿到确定高度，自己只滚内容框。 */}
      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        {/* key 一换就重挂载：面板内容淡入，切页签不是"瞬间换脸"。 */}
        <div key={tab} className="anim-fade-in flex min-h-0 flex-1 flex-col">
          <Slot name={`main.${tab}`} fallback={EmptyPanel} />
        </div>
      </div>
    </main>
  );
}
