import { useEffect, useMemo, useState } from 'react';
import { FolderOpen, RefreshCw } from 'lucide-react';
import { getBridge } from '../bridge';
import type { JavaRuntime, JavaSource } from '../bridge/types';
import { Button, SegmentedControl, TextLink } from '../primitives';
import { useUI } from '../store/ui';
import { cx } from '../lib/util';

const SOURCE_LABELS: Record<JavaSource, string> = {
  settings: '设置里指定',
  java_home: 'JAVA_HOME',
  mojang_launcher: '官方启动器自带',
  game_runtime: '游戏目录 runtime',
  registry: '注册表',
  common_dir: '已安装目录',
  path_var: 'PATH',
  custom: '自定义路径',
};

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** 同一个可执行文件只保留一条；路径比较用大小写无关的规范化形式。 */
const keyOf = (path: string) => path.replace(/\//g, '\\').toLowerCase();

export interface JavaPickerProps {
  /** 版本 JSON 要求的 Java 主版本（`null` = 没写，自动匹配就是"最高的那个"）。 */
  major: number | null;
  /** 当前设置里的 javaPath；`null` 表示自动匹配。 */
  value: string | null;
  onChange: (path: string | null) => void;
  /** 无障碍标签前缀，例如「全局」。 */
  scope?: string;
}

/**
 * Java 运行时选择器（自动 / 手动）。
 *
 * 手动模式下面列出**已经扫到的**运行时让用户直接点，最后一行留给"我就要
 * 用某个奇怪位置的 java"：浏览文件夹 → 宿主校验 `release` → 通过才落盘。
 * 选中的路径会写回调用方的设置（版本级或全局），启动时 `java_pick` 见到
 * 显式路径就不再扫描缓存。
 */
export function JavaPicker({ major, value, onChange, scope }: JavaPickerProps) {
  const showToast = useUI((s) => s.showToast);

  const [list, setList] = useState<JavaRuntime[]>([]);
  const [scanning, setScanning] = useState(false);
  const [listError, setListError] = useState<string | null>(null);

  const [auto, setAuto] = useState<JavaRuntime | null>(null);
  const [autoError, setAutoError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  const manual = value !== null;
  const prefix = scope ? `${scope} Java` : 'Java';

  const scan = async (force: boolean) => {
    setScanning(true);
    try {
      const runtimes = await getBridge().invoke<JavaRuntime[]>('java_list', { force });
      setList(runtimes.filter((r) => r.valid));
      setListError(null);
    } catch (e) {
      setListError(errText(e));
    } finally {
      setScanning(false);
    }
  };

  // 候选列表只在挂载时扫一次（宿主有 5 分钟缓存，点刷新才穿透）
  useEffect(() => {
    void scan(false);
  }, []);

  // 自动模式：问宿主"按这个主版本会挑哪个"
  useEffect(() => {
    if (manual) return;
    let cancelled = false;
    void getBridge()
      .invoke<JavaRuntime | null>('java_pick', { major })
      .then((picked) => {
        if (cancelled) return;
        setAuto(picked);
        setAutoError(null);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setAuto(null);
        setAutoError(errText(e));
      });
    return () => {
      cancelled = true;
    };
  }, [major, manual]);

  const selectedKey = useMemo(() => (value ? keyOf(value) : null), [value]);
  const customOnly =
    selectedKey !== null && !list.some((r) => keyOf(r.path) === selectedKey);

  const browse = async () => {
    const bridge = getBridge();
    if (!bridge.pickFolder) {
      showToast('当前外壳不支持浏览文件夹，可以直接填写路径');
      return;
    }
    const dir = await bridge.pickFolder();
    if (!dir) return;
    setChecking(true);
    try {
      // 用户可能选了 JDK 根、bin 目录，也可能是别的文件夹：交给宿主判断
      const runtime = await getBridge().invoke<JavaRuntime>('java_inspect', { path: dir });
      if (!runtime.valid) {
        showToast(`这个目录不像 Java：${runtime.home}`);
        return;
      }
      onChange(runtime.path);
      // 新路径可能不在缓存列表里，补进去省得看着像"没生效"
      setList((prev) =>
        prev.some((r) => keyOf(r.path) === keyOf(runtime.path)) ? prev : [runtime, ...prev],
      );
      showToast(`已指定 ${runtime.name}`);
    } catch (e) {
      showToast(errText(e));
    } finally {
      setChecking(false);
    }
  };

  const autoText = () => {
    if (autoError) return `自动匹配失败：${autoError}`;
    if (!auto) return major === null ? '本机没有可用的 Java' : `本机没有可用的 Java ${major}`;
    return `${auto.name}（${SOURCE_LABELS[auto.source] ?? auto.source}）`;
  };

  return (
    <div className="flex w-full flex-col items-end gap-2">
      <SegmentedControl
        ariaLabel={`${prefix} 运行时来源`}
        value={manual ? 'manual' : 'auto'}
        options={[
          { value: 'auto', label: '自动匹配' },
          { value: 'manual', label: '手动指定' },
        ]}
        onChange={(mode) => {
          if (mode === 'auto') {
            onChange(null);
            return;
          }
          // 切到手动：先填一个已经扫到的，用户不喜欢再改
          const first = list[0];
          if (first) onChange(first.path);
          else void browse();
        }}
      />

      {manual ? (
        <div className="w-full">
          <div className="max-h-[188px] overflow-y-auto rounded-[6px] border border-line bg-layer1">
            {list.length === 0 ? (
              <p className="px-2.5 py-2 text-[11px] text-ink4">
                {scanning ? '正在扫描本机的 Java…' : '没有扫到可用的 Java'}
              </p>
            ) : null}
            {list.map((runtime) => {
              const active = keyOf(runtime.path) === selectedKey;
              return (
                <button
                  key={runtime.path}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  onClick={() => onChange(runtime.path)}
                  className={cx(
                    'flex w-full items-start gap-2 border-b border-line px-2.5 py-1.5 text-left last:border-b-0',
                    active ? 'bg-hover' : 'hover:bg-hover',
                  )}
                >
                  <span
                    aria-hidden="true"
                    className={cx(
                      'mt-[3px] h-[11px] w-[11px] shrink-0 rounded-full border',
                      active ? 'border-accent bg-accent' : 'border-line-strong',
                    )}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[11.5px] text-ink2">
                      {runtime.name}
                      <span className="ml-1.5 text-[10px] text-ink4">
                        Java {runtime.major} · {SOURCE_LABELS[runtime.source] ?? runtime.source}
                      </span>
                    </span>
                    <span className="block truncate font-mono text-[10px] text-ink4">
                      {runtime.path}
                    </span>
                  </span>
                </button>
              );
            })}
            {customOnly ? (
              <div className="flex items-start gap-2 border-t border-line px-2.5 py-1.5">
                <span
                  aria-hidden="true"
                  className="mt-[3px] h-[11px] w-[11px] shrink-0 rounded-full border border-accent bg-accent"
                />
                <span className="min-w-0 flex-1">
                  <span className="block text-[11.5px] text-ink2">
                    自定义路径<span className="ml-1.5 text-[10px] text-ink4">（不在扫描结果里）</span>
                  </span>
                  <span className="block break-all font-mono text-[10px] text-ink4">{value}</span>
                </span>
              </div>
            ) : null}
          </div>

          <div className="mt-1.5 flex items-center gap-2">
            <Button
              variant="secondary"
              size="sm"
              icon={<FolderOpen size={11} />}
              disabled={checking || scanning}
              onClick={() => void browse()}
            >
              浏览文件夹…
            </Button>
            <Button
              variant="ghost"
              size="sm"
              icon={<RefreshCw size={11} className={cx(scanning && 'animate-spin')} />}
              disabled={scanning}
              onClick={() => void scan(true)}
            >
              重新扫描
            </Button>
            <TextLink className="ml-auto" onClick={() => void browse()}>
              自己指定…
            </TextLink>
          </div>
          {listError ? (
            <p className="mt-1 text-[10.5px] text-ink4">扫描失败：{listError}</p>
          ) : (
            <p className="mt-1 text-[10.5px] text-ink4">
              只在点击「浏览文件夹…」时校验所选的目录（读 <span className="font-mono">release</span>{' '}
              文件，不会运行 java）。
            </p>
          )}
        </div>
      ) : (
        <span className="max-w-full truncate text-[11.5px] text-ink3" title={auto?.path ?? ''}>
          自动匹配：
          <span className={cx('ml-1', auto && 'text-ink2')}>{autoText()}</span>
        </span>
      )}
    </div>
  );
}
