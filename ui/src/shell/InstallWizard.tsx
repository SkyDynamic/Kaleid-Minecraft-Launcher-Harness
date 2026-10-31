import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowLeft,
  Check,
  ChevronDown,
  ChevronRight,
  Loader2,
  PackagePlus,
  RefreshCw,
  Search,
} from 'lucide-react';
import { getBridge } from '../bridge';
import { Dialog } from '../components/Dialog';
import { Button, Collapse, Input } from '../primitives';
import { useApp } from '../store/app';
import { useSettings } from '../store/settings';
import { useUI } from '../store/ui';
import { LOADER_ICONS, VERSION_ICONS } from '../lib/assets';
import { cx, formatDateTime } from '../lib/util';
import type {
  FabricApiInfo,
  InstallLoader,
  InstallQueued,
  LoaderVersionTimes,
  LoaderVersions,
  RemoteVersion,
  RemoteVersions,
  VersionNameCheck,
} from '../bridge/types';

type Step = 1 | 2 | 3;
type LoaderChoice = 'vanilla' | 'fabric' | 'forge' | 'neoforge';

const GROUP_META: Array<{
  key: RemoteVersion['category'];
  title: string;
  hint: string;
}> = [
  { key: 'release', title: '正式版', hint: '稳定发布版本' },
  { key: 'snapshot', title: '预览版', hint: '快照与预发布' },
  { key: 'ancient', title: '远古版', hint: 'Alpha / Beta 时代' },
  { key: 'april_fools', title: '愚人节版', hint: 'Mojang 的年度玩笑' },
];

function loaderLabel(loader: LoaderChoice): string {
  if (loader === 'fabric') return 'Fabric';
  if (loader === 'forge') return 'Forge';
  if (loader === 'neoforge') return 'NeoForge';
  return '原版';
}

function defaultVersionName(game: string, loader: LoaderChoice, version: string | null): string {
  if (loader === 'vanilla' || !version) return game;
  return `${game}-${loaderLabel(loader)} ${version}`;
}

function localNameError(name: string): string | null {
  if (!name) return '版本名称不能为空';
  if (name !== name.trim()) return '名称首尾不能有空格';
  if (name.endsWith('.')) return '文件名不能以小数点结尾';
  if (name === '.' || name === '..') return '版本名称不合法';
  if (name.length > 64) return '名称不能超过 64 个字符';
  if (/[<>:"/\\|?*]/.test(name)) return '名称不能包含 < > : " / \\ | ? *';
  return null;
}

function versionIcon(version: RemoteVersion | null, loader: LoaderChoice) {
  if (loader !== 'vanilla') return LOADER_ICONS[loader];
  return VERSION_ICONS[version?.category ?? 'release'];
}

function VersionRow({
  version,
  selected,
  installed,
  onSelect,
}: {
  version: RemoteVersion;
  selected: boolean;
  installed: boolean;
  onSelect: () => void;
}) {
  const subtitle =
    version.category === 'april_fools' && version.aprilFoolsYear
      ? `${version.aprilFoolsYear} | ${version.aprilFoolsIntro ?? '愚人节版本'}`
      : formatDateTime(version.releaseTime);
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cx(
        'flex w-full items-center gap-2.5 rounded-[7px] border px-2.5 py-2 text-left transition-colors',
        selected
          ? 'border-accent/60 bg-accent/10'
          : 'border-transparent hover:border-line-strong hover:bg-hover',
      )}
    >
      <img
        src={VERSION_ICONS[version.category]}
        alt=""
        draggable={false}
        className="h-[32px] w-[32px] shrink-0 rounded-none object-contain [image-rendering:auto]"
      />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5">
          <span className="truncate text-[12px] font-medium leading-[16px] text-ink">
            {version.id}
          </span>
          {installed ? (
            <span className="shrink-0 rounded-full border border-green/30 bg-green/10 px-1.5 py-[1px] text-[8.5px] leading-[12px] text-green">
              已安装
            </span>
          ) : null}
        </span>
        <span className="mt-0.5 block truncate text-[10px] leading-[14px] text-ink4">{subtitle}</span>
      </span>
      {selected ? <Check size={13} className="shrink-0 text-accent" /> : null}
    </button>
  );
}

function GroupHeader({
  title,
  hint,
  count,
  open,
  onToggle,
  disabled,
}: {
  title: string;
  hint: string;
  count: number;
  open: boolean;
  onToggle: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      aria-disabled={disabled}
      disabled={disabled}
      className={cx(
        'flex w-full items-center gap-2 rounded-[7px] px-2 py-1.5 text-left transition-colors',
        disabled ? 'cursor-not-allowed opacity-45' : 'hover:bg-hover',
      )}
    >
      {open ? <ChevronDown size={13} className="text-ink4" /> : <ChevronRight size={13} className="text-ink4" />}
      <span className="text-[11.5px] font-semibold text-ink2">{title}</span>
      <span className="text-[9.5px] text-ink4">{hint}</span>
      <span className="ml-auto font-mono text-[9.5px] text-ink4">{count}</span>
    </button>
  );
}

function LoaderRow({
  icon,
  title,
  subtitle,
  selected,
  recommended,
  disabled,
  onSelect,
}: {
  icon: string;
  title: string;
  subtitle: string;
  selected: boolean;
  recommended?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      disabled={disabled}
      title={selected ? '再次点击返回原版' : undefined}
      aria-pressed={selected}
      className={cx(
        'flex w-full items-center gap-2.5 rounded-[6px] border px-2 py-1.5 text-left transition-colors',
        selected
          ? 'border-accent/60 bg-accent/10'
          : 'border-transparent hover:border-line-strong hover:bg-hover',
        disabled && 'cursor-not-allowed opacity-45',
      )}
    >
      <img
        src={icon}
        alt=""
        draggable={false}
        className="h-[24px] w-[24px] shrink-0 rounded-none object-contain [image-rendering:auto]"
      />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5">
          <span className="truncate font-mono text-[11px] leading-[15px] text-ink">{title}</span>
          {recommended ? (
            <span className="shrink-0 rounded-full border border-accent/30 bg-accent/10 px-1.5 py-[1px] text-[8.5px] leading-[12px] text-accent-hover">
              推荐
            </span>
          ) : null}
        </span>
        <span className="mt-0.5 block truncate text-[9.5px] leading-[13px] text-ink4">{subtitle}</span>
      </span>
      {selected ? <Check size={12} className="shrink-0 text-accent" /> : null}
    </button>
  );
}

function Steps({ step, onSelect }: { step: Step; onSelect: (step: Step) => void }) {
  const items = ['选择版本', '加载器', '确认安装'];
  return (
    <div className="flex items-center gap-1.5">
      {items.map((label, index) => {
        const number = (index + 1) as Step;
        const done = number < step;
        const active = number === step;
        return (
          <div key={label} className="flex items-center gap-1.5">
            <button
              type="button"
              disabled={number >= step}
              aria-current={active ? 'step' : undefined}
              onClick={() => onSelect(number)}
              className={cx(
                'flex items-center gap-1.5 rounded-[5px] px-1 py-0.5 transition-colors',
                done ? 'hover:bg-hover' : 'cursor-default',
              )}
            >
              <span
                className={cx(
                  'flex h-[20px] w-[20px] items-center justify-center rounded-full border text-[10px] font-semibold',
                  done
                    ? 'border-accent/60 bg-accent text-white'
                    : active
                      ? 'border-accent/60 bg-accent/15 text-accent-hover'
                      : 'border-line-strong bg-layer2 text-ink4',
                )}
              >
                {done ? <Check size={11} /> : number}
              </span>
              <span
                className={cx(
                  'text-[10.5px] leading-none',
                  active ? 'font-semibold text-ink' : done ? 'text-ink3' : 'text-ink4',
                )}
              >
                {label}
              </span>
            </button>
            {index < items.length - 1 ? <span className="mx-0.5 h-px w-4 bg-line-strong" /> : null}
          </div>
        );
      })}
    </div>
  );
}

export function InstallWizard() {
  const open = useUI((s) => s.installOpen);
  const closeInstall = useUI((s) => s.closeInstall);
  const setQueueOpen = useUI((s) => s.setQueueOpen);
  const showToast = useUI((s) => s.showToast);
  const activeRootId = useApp((s) => s.activeRootId);
  const roots = useApp((s) => s.roots);
  const versions = useApp((s) => s.versions);
  const refreshVersions = useApp((s) => s.refreshVersions);
  const globalSettings = useSettings((s) => s.global);

  const [step, setStep] = useState<Step>(1);
  const [remote, setRemote] = useState<RemoteVersions | null>(null);
  const [loadingRemote, setLoadingRemote] = useState(false);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState<Record<string, boolean>>({
    release: true,
    snapshot: true,
    ancient: false,
    april_fools: false,
  });
  const [selected, setSelected] = useState<RemoteVersion | null>(null);

  const [fabricData, setFabricData] = useState<LoaderVersions | null>(null);
  const [forgeData, setForgeData] = useState<LoaderVersions | null>(null);
  const [neoData, setNeoData] = useState<LoaderVersions | null>(null);
  const [loadingLoaders, setLoadingLoaders] = useState(false);
  const [loaderError, setLoaderError] = useState<string | null>(null);
  const [loaderChoice, setLoaderChoice] = useState<LoaderChoice>('vanilla');
  const [loaderVersion, setLoaderVersion] = useState<string | null>(null);
  const [loaderTimes, setLoaderTimes] = useState<Record<string, Record<string, string | null>>>({});
  const [timesLoading, setTimesLoading] = useState<Record<string, boolean>>({});
  const selectedIdRef = useRef<string | null>(null);
  const timesRequested = useRef<Set<string>>(new Set());
  selectedIdRef.current = selected?.id ?? null;
  const [expandedLoaders, setExpandedLoaders] = useState<Record<string, boolean>>({
    vanilla: true,
    fabric: false,
    forge: false,
    neoforge: false,
  });
  const [fabricApi, setFabricApi] = useState(true);
  const [fabricApiInfo, setFabricApiInfo] = useState<FabricApiInfo | null>(null);
  const [fabricApiLoading, setFabricApiLoading] = useState(false);

  const [name, setName] = useState('');
  const [nameTouched, setNameTouched] = useState(false);
  const [nameError, setNameError] = useState<string | null>(null);
  const [checkingName, setCheckingName] = useState(false);
  const checkSeq = useRef(0);
  const checkTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  /** 只在本轮进入第二步时预选一次已有加载器；之后允许用户改选原版。 */
  const presetLoaderFor = useRef<string | null>(null);

  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const activeRoot = roots.find((r) => r.id === activeRootId) ?? null;

  const existingLoader = useMemo(() => {
    if (!selected) return null;
    return (
      versions.find(
        (v) => v.inheritsFrom === selected.id && v.loader !== 'vanilla' && v.loader !== 'unrecognized',
      ) ?? null
    );
  }, [selected, versions]);

  const lockedLoader: LoaderChoice | null = existingLoader
    ? (existingLoader.loader as LoaderChoice)
    : loaderChoice !== 'vanilla'
      ? loaderChoice
      : null;

  const reset = () => {
    setStep(1);
    setQuery('');
    setExpanded({ release: true, snapshot: true, ancient: false, april_fools: false });
    setSelected(null);
    setFabricData(null);
    setForgeData(null);
    setNeoData(null);
    setLoaderError(null);
    setLoaderChoice('vanilla');
    setLoaderVersion(null);
    setLoaderTimes({});
    setTimesLoading({});
    timesRequested.current.clear();
    setExpandedLoaders({ vanilla: true, fabric: false, forge: false, neoforge: false });
    setFabricApi(true);
    setFabricApiInfo(null);
    setName('');
    setNameTouched(false);
    setNameError(null);
    setSubmitError(null);
    setSubmitting(false);
    presetLoaderFor.current = null;
  };

  const loadRemote = async (force = false) => {
    setLoadingRemote(true);
    setRemoteError(null);
    try {
      const data = await getBridge().invoke<RemoteVersions>('versions_remote', { force });
      setRemote(data);
    } catch (error) {
      setRemoteError(error instanceof Error ? error.message : String(error));
    } finally {
      setLoadingRemote(false);
    }
  };

  useEffect(() => {
    if (!open) return;
    reset();
    void loadRemote(false);
    // reset/loadRemote 只在打开时执行一次；它们读取的都是最新 state。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 进入第二步时并发加载 Fabric / Forge / NeoForge，任何一个慢都不阻塞其他两个。
  useEffect(() => {
    if (!selected || step !== 2) return;
    let cancelled = false;
    setLoadingLoaders(true);
    setLoaderError(null);
    const bridge = getBridge();
    const load = (loader: LoaderChoice) =>
      bridge.invoke<LoaderVersions>('loaders_versions', {
        gameVersion: selected.id,
        loader,
      });
    Promise.allSettled([load('fabric'), load('forge'), load('neoforge')]).then(
      ([fabric, forge, neo]) => {
        if (cancelled) return;
        setFabricData(fabric.status === 'fulfilled' ? fabric.value : null);
        setForgeData(forge.status === 'fulfilled' ? forge.value : null);
        setNeoData(neo.status === 'fulfilled' ? neo.value : null);
        if (
          fabric.status === 'rejected' &&
          forge.status === 'rejected' &&
          neo.status === 'rejected'
        ) {
          setLoaderError('加载器列表获取失败，可先安装原版，稍后重试。');
        }
        setLoadingLoaders(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [selected, step]);

  // 进入第二步时，若同版本已有加载器则预选一次；之后仍允许改选原版另建。
  useEffect(() => {
    if (step !== 2 || !selected) return;
    if (existingLoader && presetLoaderFor.current !== selected.id) {
      presetLoaderFor.current = selected.id;
      setLoaderChoice(existingLoader.loader as LoaderChoice);
      setLoaderVersion(existingLoader.loaderVersion);
      setExpandedLoaders((prev) => ({
        ...prev,
        [existingLoader.loader]: true,
      }));
      return;
    }
    if (loaderChoice === 'vanilla') {
      setLoaderVersion(null);
    }
  }, [existingLoader, selected, step, loaderChoice]);

  // Fabric API 信息。
  useEffect(() => {
    if (!selected || loaderChoice !== 'fabric') {
      setFabricApiInfo(null);
      return;
    }
    let cancelled = false;
    setFabricApiLoading(true);
    getBridge().invoke<FabricApiInfo | null>('fabric_api_latest', {
      gameVersion: selected.id,
    })
      .then((info) => {
        if (!cancelled) setFabricApiInfo(info);
      })
      .catch(() => {
        if (!cancelled) setFabricApiInfo(null);
      })
      .finally(() => {
        if (!cancelled) setFabricApiLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [selected, loaderChoice]);

  // 任一加载器分组展开时都懒加载版本时间；不依赖用户先选中某个版本。
  useEffect(() => {
    if (step < 2 || !selected) return;
    const bridge = getBridge();
    const gameId = selected.id;
    const choices: LoaderChoice[] = ['fabric', 'forge', 'neoforge'];
    for (const choice of choices) {
      if (!expandedLoaders[choice] || loaderTimes[choice] || timesLoading[choice]) continue;
      const requestKey = `${gameId}:${choice}`;
      if (timesRequested.current.has(requestKey)) continue;
      const data =
        choice === 'fabric' ? fabricData : choice === 'forge' ? forgeData : neoData;
      if (!data || data.versions.length === 0) continue;
      timesRequested.current.add(requestKey);
      const list = data.versions.slice(0, 20).map((v) => v.version);
      setTimesLoading((prev) => ({ ...prev, [choice]: true }));
      bridge
        .invoke<LoaderVersionTimes>('loader_version_times', {
          gameVersion: gameId,
          loader: choice,
          versions: list,
        })
        .then((times) => {
          if (selectedIdRef.current !== gameId) return;
          setLoaderTimes((prev) => ({ ...prev, [choice]: times.times }));
        })
        .catch(() => {
          if (selectedIdRef.current !== gameId) return;
          setLoaderTimes((prev) => ({ ...prev, [choice]: {} }));
        })
        .finally(() => {
          setTimesLoading((prev) => ({ ...prev, [choice]: false }));
        });
    }
  }, [step, selected, expandedLoaders, fabricData, forgeData, neoData, loaderTimes, timesLoading]);

  // 切换游戏版本时清空上一轮时间，避免旧缓存挡住新版本。
  useEffect(() => {
    setLoaderTimes({});
    setTimesLoading({});
    timesRequested.current.clear();
  }, [selected?.id]);

  // 默认名称：未手动改过时跟随游戏版本/加载器/加载器版本。
  useEffect(() => {
    if (nameTouched || !selected) return;
    setName(defaultVersionName(selected.id, loaderChoice, loaderVersion));
  }, [selected, loaderChoice, loaderVersion, nameTouched]);

  // 名称校验：本地规则立即反馈，重名/保留名交给后端 250ms 防抖。
  useEffect(() => {
    if (!name || !activeRootId) {
      setNameError(null);
      setCheckingName(false);
      return;
    }
    const local = localNameError(name);
    if (local) {
      setNameError(local);
      setCheckingName(false);
      return;
    }
    setCheckingName(true);
    if (checkTimer.current) clearTimeout(checkTimer.current);
    const seq = ++checkSeq.current;
    checkTimer.current = setTimeout(() => {
      getBridge().invoke<VersionNameCheck>('versions_check_name', {
        rootId: activeRootId,
        name,
      })
        .then((result) => {
          if (seq !== checkSeq.current) return;
          setNameError(result.ok ? null : result.error);
        })
        .catch((error) => {
          if (seq === checkSeq.current) setNameError(error instanceof Error ? error.message : String(error));
        })
        .finally(() => {
          if (seq === checkSeq.current) setCheckingName(false);
        });
    }, 250);
    return () => {
      if (checkTimer.current) clearTimeout(checkTimer.current);
    };
  }, [name, activeRootId]);

  const groupVersions = useMemo(() => {
    const all = remote?.versions ?? [];
    const needle = query.trim().toLowerCase();
    const filter = (category: RemoteVersion['category']) =>
      all.filter((version) => {
        if (version.category !== category) return false;
        if (!needle) return true;
        return (
          version.id.toLowerCase().includes(needle) ||
          (version.aprilFoolsIntro ?? '').toLowerCase().includes(needle)
        );
      });
    return GROUP_META.map((group) => ({ ...group, versions: filter(group.key) }));
  }, [remote, query]);

  const visibleGroups = query.trim()
    ? groupVersions.filter((group) => group.versions.length > 0)
    : groupVersions;

  const selectVersion = (version: RemoteVersion) => {
    setSelected(version);
    presetLoaderFor.current = null;
    setNameTouched(false);
    setLoaderChoice('vanilla');
    setLoaderVersion(null);
    setFabricData(null);
    setForgeData(null);
    setNeoData(null);
    setLoaderTimes({});
    setFabricApiInfo(null);
  };

  const chooseLoader = (choice: LoaderChoice, version: string | null) => {
    // 再次点击已选中的加载器版本 → 回到原版；展开分组本身不选中任何加载器。
    if (choice !== 'vanilla' && choice === loaderChoice && version === loaderVersion) {
      setLoaderChoice('vanilla');
      setLoaderVersion(null);
      // 只回到原版，不重置其它分组的展开状态，避免列表突然折叠。
      setExpandedLoaders((prev) => ({ ...prev, vanilla: true }));
      return;
    }
    setLoaderChoice(choice);
    setLoaderVersion(version);
    setExpandedLoaders({
      vanilla: choice === 'vanilla',
      fabric: choice === 'fabric',
      forge: choice === 'forge',
      neoforge: choice === 'neoforge',
    });
    if (choice === 'fabric') setFabricApi(true);
  };

  const toggleLoaderGroup = (choice: LoaderChoice) => {
    if (lockedLoader && choice !== 'vanilla' && choice !== lockedLoader) return;
    setExpandedLoaders((prev) => ({ ...prev, [choice]: !prev[choice] }));
  };

  const submit = async () => {
    if (!selected || !activeRoot) return;
    if (nameError) {
      setSubmitError(nameError);
      return;
    }
    setSubmitting(true);
    setSubmitError(null);
    try {
      const loader: InstallLoader =
        loaderChoice === 'vanilla' || !loaderVersion
          ? { kind: 'vanilla' }
          : loaderChoice === 'fabric'
            ? { kind: 'fabric', version: loaderVersion }
            : loaderChoice === 'forge'
              ? { kind: 'forge', version: loaderVersion }
              : { kind: 'neoforge', version: loaderVersion };
      await getBridge().invoke<InstallQueued>('versions_install', {
        rootId: activeRoot.id,
        gameVersion: selected.id,
        versionName: name.trim(),
        loader,
        installFabricApi: loaderChoice === 'fabric' && fabricApi,
      });
      closeInstall();
      showToast(`已加入下载队列：${name.trim()}`);
      await refreshVersions();
      setQueueOpen(true);
    } catch (error) {
      setSubmitError(error instanceof Error ? error.message : String(error));
    } finally {
      setSubmitting(false);
    }
  };

  const canNext =
    step === 1
      ? Boolean(selected)
      : step === 2
        ? Boolean(name.trim()) && !nameError && (loaderChoice === 'vanilla' || Boolean(loaderVersion))
        : true;

  const selectedIcon = versionIcon(selected, loaderChoice);
  const nameOk = Boolean(name.trim()) && !nameError && !checkingName;
  const downloadSource = globalSettings?.downloadSource ?? 'official-first';
  const sourceLabel =
    downloadSource === 'mirror-first'
      ? 'BMCLAPI → 官方'
      : downloadSource === 'mirror-only'
        ? '仅 BMCLAPI'
        : downloadSource === 'official-only'
          ? '仅官方源'
          : '官方优先 → BMCLAPI';
  const sourceHint =
    downloadSource === 'mirror-first'
      ? 'BMCLAPI 优先；文件失败时回退官方源'
      : downloadSource === 'mirror-only'
        ? '只使用 BMCLAPI 镜像；官方源不会被访问'
        : downloadSource === 'official-only'
          ? '仅使用官方源；文件失败时不会自动切换镜像'
          : '官方源优先；连续两个测速窗口低于 512 KB/s 时自动切换 BMCLAPI';

  return (
    <Dialog
      open={open}
      onClose={closeInstall}
      title="获取新版本"
      subtitle={sourceHint}
      width={860}
      bodyClassName="min-h-0"
      footer={
        <div className="flex items-center justify-between gap-3">
          <div className="min-w-0 text-[10px] leading-[14px] text-ink4">
            {activeRoot ? `安装到 ${activeRoot.alias}` : '还没有选择游戏文件夹'}
          </div>
          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={closeInstall}>
              取消
            </Button>
            {step > 1 ? (
              <Button variant="secondary" onClick={() => setStep((step - 1) as Step)}>
                上一步
              </Button>
            ) : null}
            {step < 3 ? (
              <Button
                variant="primary"
                disabled={!canNext || !activeRootId}
                onClick={() => setStep((step + 1) as Step)}
              >
                下一步
              </Button>
            ) : (
              <Button
                variant="primary"
                disabled={submitting || !nameOk || !activeRootId}
                icon={submitting ? <Loader2 size={13} className="animate-spin" /> : <PackagePlus size={13} />}
                onClick={() => void submit()}
              >
                {submitting ? '正在创建任务…' : '开始安装'}
              </Button>
            )}
          </div>
        </div>
      }
    >
      <div className="flex min-h-[520px] flex-col">
        <div className="flex items-center justify-between border-b border-line px-4 py-3">
          <Steps step={step} onSelect={setStep} />
          {step === 1 ? (
            <div className="flex items-center gap-2">
              <div className="relative">
                <Search size={12} className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-ink4" />
                <Input
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="搜索版本"
                  className="h-[26px] w-[150px] pl-6 text-[11px]"
                  aria-label="搜索版本"
                />
              </div>
            </div>
          ) : (
            <span className="text-[10px] text-ink4">
              {selected ? `${selected.id} · ${loaderLabel(loaderChoice)}` : ''}
            </span>
          )}
        </div>

        {!activeRoot ? (
          <div className="m-4 rounded-[8px] border border-amber/30 bg-amber/10 px-3 py-2 text-[11.5px] leading-[17px] text-amber">
            还没有游戏文件夹。请先在左侧添加一个 `.minecraft` 目录，再回来安装。
          </div>
        ) : null}

        {step === 1 ? (
          <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
            {loadingRemote && !remote ? (
              <div className="flex items-center justify-center gap-2 py-16 text-[11.5px] text-ink4">
                <Loader2 size={14} className="animate-spin" />
                正在读取 Mojang 版本清单…
              </div>
            ) : remoteError ? (
              <div className="flex flex-col items-center gap-3 py-14">
                <p className="text-[11.5px] text-red">{remoteError}</p>
                <Button variant="secondary" size="sm" icon={<RefreshCw size={12} />} onClick={() => void loadRemote(true)}>
                  重试
                </Button>
              </div>
            ) : visibleGroups.length === 0 ? (
              <p className="py-14 text-center text-[11.5px] text-ink4">
                {remote?.versions.length === 0 ? '版本清单为空' : '没有匹配的版本'}
              </p>
            ) : (
              <div className="space-y-1.5">
                {visibleGroups.map((group) => {
                  const openGroup = query.trim() ? true : expanded[group.key];
                  return (
                    <section key={group.key} className="rounded-[8px] border border-line bg-layer2/25">
                      <GroupHeader
                        title={group.title}
                        hint={group.hint}
                        count={group.versions.length}
                        open={openGroup}
                        onToggle={() =>
                          setExpanded((prev) => ({ ...prev, [group.key]: !prev[group.key] }))
                        }
                      />
                      <Collapse open={Boolean(openGroup)}>
                        <div className="grid max-h-[240px] grid-cols-2 gap-1 overflow-y-auto px-1.5 pb-1.5">
                          {group.versions.map((version) => (
                            <VersionRow
                              key={version.id}
                              version={version}
                              selected={selected?.id === version.id}
                              installed={versions.some((v) => v.id === version.id)}
                              onSelect={() => selectVersion(version)}
                            />
                          ))}
                        </div>
                      </Collapse>
                    </section>
                  );
                })}
              </div>
            )}
          </div>
        ) : null}

        {step === 2 && selected ? (
          <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
            <div className="rounded-[9px] border border-line bg-layer2/40 p-2.5">
              <div className="flex items-center gap-2.5">
                <button
                  type="button"
                  aria-label="返回选择版本"
                  onClick={() => setStep(1)}
                  className="flex h-[28px] w-[28px] shrink-0 items-center justify-center rounded-[6px] text-ink4 hover:bg-hover hover:text-ink"
                >
                  <ArrowLeft size={16} />
                </button>
                <img
                  src={selectedIcon}
                  alt=""
                  className="h-[32px] w-[32px] shrink-0 rounded-none object-contain [image-rendering:auto]"
                />
                <div className="min-w-0 flex-1">
                  <p className="text-[10px] leading-[14px] text-ink4">版本名称（文件夹名）</p>
                  <Input
                    value={name}
                    onChange={(event) => {
                      setNameTouched(true);
                      setName(event.target.value);
                    }}
                    aria-label="版本名称"
                    aria-invalid={Boolean(nameError)}
                    className={cx('mt-0.5 h-[28px] text-left font-mono text-[12px]', nameError && 'border-red/60')}
                  />
                </div>
              </div>
              {nameError ? (
                <p className="mt-2 pl-[38px] text-[10.5px] leading-[15px] text-red">{nameError}</p>
              ) : checkingName ? (
                <p className="mt-2 pl-[38px] text-[10px] leading-[15px] text-ink4">正在校验名称…</p>
              ) : null}
            </div>

            {existingLoader ? (
              <div className="mt-2 rounded-[7px] border border-amber/25 bg-amber/10 px-2.5 py-1.5 text-[10.5px] leading-[15px] text-amber">
                该游戏版本已安装 {loaderLabel(existingLoader.loader as LoaderChoice)}{' '}
                {existingLoader.loaderVersion}；只能继续使用同一种加载器，或选择原版另建一个版本。
              </div>
            ) : null}

            <div className="mt-3 space-y-1.5">
              <section className="rounded-[8px] border border-line bg-layer2/25">
                <GroupHeader
                  title="原版"
                  hint="不安装模组加载器"
                  count={1}
                  open={expandedLoaders.vanilla}
                  onToggle={() => toggleLoaderGroup('vanilla')}
                />
                <Collapse open={Boolean(expandedLoaders.vanilla)}>
                  <div className="px-1.5 pb-1.5">
                    <LoaderRow
                      icon={VERSION_ICONS[selected.category]}
                      title="Minecraft 原版"
                      subtitle="仅安装游戏本体与资源"
                      selected={loaderChoice === 'vanilla'}
                      onSelect={() => chooseLoader('vanilla', null)}
                    />
                  </div>
                </Collapse>
              </section>

              {(['fabric', 'forge', 'neoforge'] as const).map((choice) => {
                const data =
                  choice === 'fabric' ? fabricData : choice === 'forge' ? forgeData : neoData;
                const disabled = Boolean(lockedLoader && lockedLoader !== choice);
                const openGroup = !disabled && expandedLoaders[choice];
                return (
                  <section
                    key={choice}
                    className={cx(
                      'rounded-[8px] border border-line bg-layer2/25',
                      disabled && 'opacity-45',
                    )}
                  >
                    <GroupHeader
                      title={loaderLabel(choice)}
                      hint={
                        choice === 'fabric'
                          ? '轻量、生态广'
                          : choice === 'forge'
                            ? '经典模组生态'
                            : 'Forge 后继'
                      }
                      count={data?.versions.length ?? 0}
                      open={openGroup}
                      disabled={disabled}
                      onToggle={() => toggleLoaderGroup(choice)}
                    />
                    <Collapse open={Boolean(openGroup)}>
                      <div className="px-1.5 pb-1.5">
                        {loadingLoaders && !data ? (
                          <div className="flex items-center gap-2 px-2 py-3 text-[10.5px] text-ink4">
                            <Loader2 size={12} className="animate-spin" />
                            正在读取 {loaderLabel(choice)} 版本…
                          </div>
                        ) : data && data.versions.length > 0 ? (
                          <div className="max-h-[190px] space-y-0.5 overflow-y-auto">
                            {data.versions.map((item) => {
                              const time = loaderTimes[choice]?.[item.version] ?? item.time;
                              const timeLabel = time
                                ? formatDateTime(time)
                                : timesLoading[choice]
                                  ? '时间加载中…'
                                  : '时间未知';
                              const subtitle = `${timeLabel} | ${item.stable ? '正式版' : '测试版'}`;
                              return (
                                <LoaderRow
                                  key={item.version}
                                  icon={LOADER_ICONS[choice]}
                                  title={item.version}
                                  subtitle={subtitle}
                                  recommended={item.recommended}
                                  selected={loaderChoice === choice && loaderVersion === item.version}
                                  onSelect={() => chooseLoader(choice, item.version)}
                                />
                              );
                            })}
                          </div>
                        ) : (
                          <p className="px-2 py-3 text-[10.5px] text-ink4">
                            该游戏版本没有可用的 {loaderLabel(choice)}。
                          </p>
                        )}
                      </div>
                    </Collapse>
                    {choice === 'fabric' && loaderChoice === 'fabric' ? (
                      <div className="mx-1.5 mb-1.5 rounded-[6px] border border-line bg-layer1/70 px-2 py-1.5">
                        <label className="flex cursor-pointer items-center gap-2">
                          <img
                            src={LOADER_ICONS.fabric}
                            alt=""
                            draggable={false}
                            className="h-[28px] w-[28px] shrink-0 rounded-none object-contain [image-rendering:auto]"
                          />
                          <input
                            type="checkbox"
                            checked={fabricApi}
                            onChange={(event) => setFabricApi(event.target.checked)}
                            className="h-[13px] w-[13px] shrink-0 accent-[var(--color-accent)]"
                          />
                          <span className="min-w-0 flex-1">
                            <span className="block text-[11px] text-ink2">
                              Fabric API
                              {fabricApiInfo ? ` ${fabricApiInfo.version}` : fabricApiLoading ? '（读取中…）' : ''}
                            </span>
                            <span className="mt-0.5 block text-[9.5px] text-ink4">
                              安装到版本生效的 mods/ 目录；已有其它 Fabric API 时跳过。
                            </span>
                          </span>
                        </label>
                      </div>
                    ) : null}
                  </section>
                );
              })}
            </div>
            {loaderError ? (
              <p className="mt-2 text-[10.5px] leading-[15px] text-amber">{loaderError}</p>
            ) : null}
          </div>
        ) : null}

        {step === 3 && selected ? (
          <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
            <div className="rounded-[10px] border border-line bg-layer2/45 p-3.5">
              <div className="flex items-center gap-3">
                <img
                  src={selectedIcon}
                  alt=""
                  className="h-[40px] w-[40px] shrink-0 rounded-none object-contain [image-rendering:auto]"
                />
                <div className="min-w-0 flex-1">
                  <p className="truncate text-[13px] font-semibold text-ink">{name || '未命名版本'}</p>
                  <p className="mt-0.5 text-[10.5px] leading-[15px] text-ink4">
                    Minecraft {selected.id} · {loaderLabel(loaderChoice)}
                    {loaderVersion ? ` ${loaderVersion}` : ''}
                  </p>
                </div>
              </div>
              <div className="mt-3 border-t border-line pt-2.5">
                <label className="text-[10px] text-ink4" htmlFor="install-final-name">
                  版本名称
                </label>
                <Input
                  id="install-final-name"
                  value={name}
                  onChange={(event) => {
                    setNameTouched(true);
                    setName(event.target.value);
                  }}
                  className={cx('mt-1 h-[30px] text-left font-mono text-[12px]', nameError && 'border-red/60')}
                  aria-invalid={Boolean(nameError)}
                />
                {nameError ? (
                  <p className="mt-1 text-[10.5px] leading-[15px] text-red">{nameError}</p>
                ) : (
                  <p className="mt-1 text-[10px] leading-[15px] text-ink4">
                    将创建 versions/{name || '…'}/ 文件夹；安装后可随时重命名。
                  </p>
                )}
              </div>
            </div>

            <dl className="mt-3 space-y-1.5 rounded-[8px] border border-line bg-layer2/25 px-3 py-2.5 text-[11px]">
              <div className="flex justify-between gap-3">
                <dt className="text-ink4">安装位置</dt>
                <dd className="truncate text-ink2">{activeRoot?.path ?? '—'}</dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-ink4">下载源</dt>
                <dd className="text-ink2">{sourceLabel}</dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-ink4">下载线程</dt>
                <dd className="text-ink2">跟随全局设置（共享线程池）</dd>
              </div>
              {loaderChoice === 'fabric' ? (
                <div className="flex justify-between gap-3">
                  <dt className="text-ink4">Fabric API</dt>
                  <dd className="text-ink2">
                    {fabricApi
                      ? fabricApiInfo
                        ? `安装 ${fabricApiInfo.version}`
                        : '安装（读取中）'
                      : '不安装'}
                  </dd>
                </div>
              ) : null}
            </dl>

            {submitError ? (
              <p className="mt-3 rounded-[7px] border border-red/25 bg-red/10 px-3 py-2 text-[11px] leading-[16px] text-red">
                {submitError}
              </p>
            ) : null}

            <p className="mt-3 text-[10px] leading-[16px] text-ink4">
              安装会向队列添加真实下载任务；关闭此窗口不会中断。可在右侧任务队列查看总进度和每个阶段。
            </p>
          </div>
        ) : null}
      </div>
    </Dialog>
  );
}
