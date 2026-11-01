import { useEffect, useRef, useState, type FormEvent, type MouseEvent, type ReactNode } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Download,
  Folder,
  FolderOpen,
  Layers,
  MoreHorizontal,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  RefreshCw,
  Search,
  Settings,
  Star,
} from 'lucide-react';
import type { IsolationMode, LoaderKind, Root, VersionSummary } from '../bridge/types';
import { Button, Collapse, IconButton, Input } from '../primitives';
import { LoaderUpdateDialog, VersionDeleteDialog } from '../components/VersionAdminDialogs';
import { MenuDivider, MenuItem, MenuLabel, Popover } from '../components/Menu';
import { getBridge } from '../bridge';
import { useApp, usePluginEnabled } from '../store/app';
import { usePrefs } from '../store/prefs';
import { useUI } from '../store/ui';
import { cx } from '../lib/util';
import { LOADER_ICONS, VERSION_ICONS } from '../lib/assets';
import { Slot } from './slots';

const ISOLATION_LABELS: Record<IsolationMode, string> = {
  inherit: '继承全局',
  on: '开启',
  off: '关闭',
};

const LOADER_GROUPS: Array<{ kind: LoaderKind; label: string }> = [
  { kind: 'vanilla', label: '原版' },
  { kind: 'fabric', label: 'Fabric' },
  { kind: 'forge', label: 'Forge' },
  { kind: 'neoforge', label: 'NeoForge' },
  // 只有低版本单装 OptiFine 会落到这一组；配合 Forge 的仍归在 Forge 下。
  { kind: 'optifine', label: 'OptiFine' },
  { kind: 'unrecognized', label: '未识别' },
];

function BrandMark({ size = 22 }: { size?: number }) {
  return (
    <div
      className="grid shrink-0 grid-cols-2 gap-[1.5px] rounded-[6px] bg-[#121214] p-[2.5px]"
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <span className="rounded-[1.5px] bg-accent" />
      <span className="rounded-[1.5px] bg-green" />
      <span className="rounded-[1.5px] bg-amber" />
      <span className="rounded-[1.5px] bg-[#c56bff]" />
    </div>
  );
}

function SectionHeader({
  title,
  count,
  action,
}: {
  title: string;
  count?: number;
  action?: ReactNode;
}) {
  return (
    <div className="flex h-[24px] items-center gap-1.5 px-3 pb-0.5 pt-1.5">
      <span className="text-[10px] font-medium uppercase tracking-[0.1em] text-ink4">{title}</span>
      {typeof count === 'number' ? (
        <span className="font-mono text-[9.5px] leading-none text-ink4">{count}</span>
      ) : null}
      {action ? <div className="ml-auto flex items-center">{action}</div> : null}
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * Primary action (slot: sidebar.header.action)
 * ------------------------------------------------------------------ */

export function PrimaryAction() {
  const openInstall = useUI((s) => s.openInstall);
  const enabled = usePluginEnabled('version-manager');
  if (!enabled) return null;
  return (
    <Button
      variant="primary"
      size="md"
      className="w-full"
      icon={<Download size={13} />}
      onClick={openInstall}
    >
      获取新版本
    </Button>
  );
}

function aliasFromPath(path: string): string {
  const parts = path.replace(/[\\/]+$/, '').split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** Fallback when the version-manager plugin is disabled: no action button. */
function ReadOnlyAction() {
  return (
    <div className="flex h-[30px] w-full select-none items-center justify-center rounded-[7px] border border-dashed border-line-strong text-[11.5px] text-ink4">
      版本管理已停用
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * Folder zone (slot: sidebar.roots)
 * ------------------------------------------------------------------ */

export function FolderZone() {
  const roots = useApp((s) => s.roots);
  const activeRootId = useApp((s) => s.activeRootId);
  const selectRoot = useApp((s) => s.selectRoot);
  const addRoot = useApp((s) => s.addRoot);
  const removeRoot = useApp((s) => s.removeRoot);
  const renameRoot = useApp((s) => s.renameRoot);
  const setRootIsolation = useApp((s) => s.setRootIsolation);
  const defaultRootId = useApp((s) => s.defaultRootId);
  const setDefaultRoot = useApp((s) => s.setDefaultRoot);
  const addOpen = useUI((s) => s.addRootOpen);
  const setAddOpen = useUI((s) => s.setAddRootOpen);
  const showToast = useUI((s) => s.showToast);

  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const [menuMode, setMenuMode] = useState<'root' | 'isolation'>('root');
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [path, setPath] = useState('');
  const [alias, setAlias] = useState('');
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const nativePicker = typeof getBridge().pickFolder === 'function';

  const openMenu = (root: Root, event: MouseEvent<HTMLButtonElement>) => {
    event.stopPropagation();
    const rect = event.currentTarget.getBoundingClientRect();
    setMenuMode('root');
    setMenu({ id: root.id, x: rect.right + 4, y: rect.top - 4 });
  };

  const startRename = (root: Root) => {
    setMenu(null);
    setRenameId(root.id);
    setRenameValue(root.alias);
    showToast('输入新别名后按 Enter 确认');
  };

  const commitRename = async (id: string) => {
    const value = renameValue.trim();
    setRenameId(null);
    if (!value) return;
    await renameRoot(id, value);
  };

  const submitAdd = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting) return;
    setSubmitting(true);
    setFormError(null);
    try {
      await addRoot(path.trim(), alias.trim() || undefined);
      setAddOpen(false);
      setPath('');
      setAlias('');
      showToast('文件夹已加入列表');
    } catch (e) {
      setFormError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const browseFolder = async () => {
    const bridge = getBridge();
    if (!bridge.pickFolder) {
      showToast('当前环境不支持原生文件夹选择器，请手动输入路径');
      return;
    }
    try {
      const picked = await bridge.pickFolder();
      if (!picked) return;
      setPath(picked);
      if (!alias.trim()) setAlias(aliasFromPath(picked));
    } catch (e) {
      setFormError(e instanceof Error ? e.message : String(e));
    }
  };

  const menuRoot = menu ? roots.find((r) => r.id === menu.id) ?? null : null;

  return (
    <div className="shrink-0 px-1.5 pb-1">
      <SectionHeader
        title="文件夹"
        count={roots.length}
        action={
          <IconButton
            label="添加文件夹"
            size={20}
            onClick={() => {
              setFormError(null);
              setAddOpen(!addOpen);
            }}
          >
            <Plus size={12} />
          </IconButton>
        }
      />

      {addOpen ? (
        <form
          onSubmit={submitAdd}
          className="mx-1.5 mb-1.5 space-y-1.5 rounded-[7px] border border-line bg-layer2 p-2"
        >
          <div className="flex items-center gap-1.5">
            <Input
              autoFocus
              readOnly={nativePicker}
              aria-label="游戏文件夹路径"
              className={cx(
                'h-[24px] min-w-0 flex-1 px-2 text-[11px]',
                nativePicker && 'cursor-pointer',
              )}
              placeholder={nativePicker ? '点击选择文件夹…' : 'D:\\Games\\MC\\.minecraft'}
              value={path}
              onClick={() => {
                if (nativePicker) void browseFolder();
              }}
              onChange={(e) => {
                if (nativePicker) return;
                const next = e.target.value;
                setPath(next);
                if (!alias.trim() && next.trim()) setAlias(aliasFromPath(next.trim()));
              }}
              onKeyDown={(e) => {
                if (e.key === 'Escape') setAddOpen(false);
              }}
            />
            <Button
              size="sm"
              variant="secondary"
              className="h-[24px] shrink-0 px-2 text-[11px]"
              icon={<FolderOpen size={11} />}
              onClick={() => void browseFolder()}
            >
              浏览
            </Button>
          </div>
          <Input
            className="h-[24px] px-2 text-[11px]"
            placeholder="别名（可选）"
            value={alias}
            onChange={(e) => setAlias(e.target.value)}
          />
          {formError ? <p className="text-[10px] leading-[1.4] text-red">{formError}</p> : null}
          <div className="flex gap-1.5">
            <Button
              type="submit"
              size="sm"
              variant="primary"
              className="h-[24px] flex-1 px-2 text-[11px]"
              disabled={submitting || !path.trim()}
            >
              添加
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-[24px] px-2 text-[11px]"
              onClick={() => setAddOpen(false)}
            >
              取消
            </Button>
          </div>
        </form>
      ) : null}

      <div className="max-h-[186px] overflow-y-auto pr-0.5">
        {roots.length === 0 ? (
          <p className="px-3 py-2 text-[11.5px] text-ink4">还没有文件夹</p>
        ) : null}
        {roots.map((root) => {
          const selected = root.id === activeRootId;
          return (
            <div
              key={root.id}
              role="button"
              tabIndex={0}
              onClick={() => void selectRoot(root.id)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') void selectRoot(root.id);
              }}
              className={cx(
                'group relative flex h-[34px] cursor-default select-none items-center gap-1.5 rounded-[6px] pl-2.5 pr-1 transition-colors',
                selected ? 'bg-hover' : 'hover:bg-hover',
              )}
            >
              {selected ? (
                <span className="absolute bottom-[3px] left-0 top-[3px] w-[2px] rounded-full bg-accent" />
              ) : null}
              {renameId === root.id ? (
                <Input
                  autoFocus
                  className="h-[24px] px-1.5 text-[11.5px]"
                  value={renameValue}
                  onChange={(e) => setRenameValue(e.target.value)}
                  onClick={(e) => e.stopPropagation()}
                  onBlur={() => void commitRename(root.id)}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === 'Enter') void commitRename(root.id);
                    if (e.key === 'Escape') setRenameId(null);
                  }}
                />
              ) : (
                <>
                  <div className="min-w-0 flex-1">
                    <div
                      className={cx(
                        'flex items-center gap-1.5 truncate text-[13px] leading-[15px]',
                        selected ? 'text-ink' : 'text-ink2 group-hover:text-ink',
                      )}
                    >
                      <span className="truncate">{root.alias}</span>
                      {root.id === defaultRootId ? (
                        <span className="flex shrink-0 items-center gap-0.5 rounded-[4px] bg-amber/15 px-1 text-[9px] leading-[14px] text-amber">
                          <Star size={7.5} className="fill-current" />
                          默认
                        </span>
                      ) : null}
                    </div>
                    <div className="truncate font-mono text-[10px] leading-[13px] text-ink4">
                      {root.path}
                    </div>
                  </div>
                  <IconButton
                    label="更多操作"
                    size={22}
                    className={cx(
                      // 悬停/键盘聚焦时才显形；键盘用户 Tab 到它时也必须看得见
                      'shrink-0 opacity-0 group-hover:opacity-100 focus-visible:opacity-100',
                      menu?.id === root.id && 'opacity-100',
                    )}
                    onClick={(e) => openMenu(root, e)}
                  >
                    <MoreHorizontal size={13} />
                  </IconButton>
                </>
              )}
            </div>
          );
        })}
      </div>

      {menu && menuRoot ? (
        <Popover x={menu.x} y={menu.y} onClose={() => setMenu(null)}>
          {menuMode === 'isolation' ? (
            <>
              <MenuLabel>版本隔离</MenuLabel>
              {(['inherit', 'on', 'off'] as IsolationMode[]).map((mode) => (
                <MenuItem
                  key={mode}
                  active={menuRoot.isolation === mode}
                  onClick={() => {
                    setMenu(null);
                    void setRootIsolation(menuRoot.id, mode);
                  }}
                >
                  {ISOLATION_LABELS[mode]}
                </MenuItem>
              ))}
            </>
          ) : (
            <>
              <MenuItem onClick={() => startRename(menuRoot)}>重命名别名</MenuItem>
              <MenuItem
                onClick={() => {
                  setMenu(null);
                  showToast(`已在文件管理器中打开：${menuRoot.path}`);
                }}
              >
                打开目录
              </MenuItem>
              <MenuItem onClick={() => setMenuMode('isolation')}>版本隔离</MenuItem>
              <MenuItem
                active={menuRoot.id === defaultRootId}
                onClick={() => {
                  setMenu(null);
                  void setDefaultRoot(menuRoot.id === defaultRootId ? null : menuRoot.id);
                }}
              >
                {menuRoot.id === defaultRootId ? '取消默认目录' : '设为默认目录'}
              </MenuItem>
              <MenuItem
                onClick={() => {
                  setMenu(null);
                  // 启动器不再每次问「用哪个目录」，换目录改成从这儿手动进
                  useApp.setState({ rootChooserOpen: true });
                }}
              >
                切换游戏目录…
              </MenuItem>
              <MenuDivider />
              <MenuItem
                danger
                onClick={() => {
                  setMenu(null);
                  void removeRoot(menuRoot.id);
                }}
              >
                移出列表
              </MenuItem>
            </>
          )}
        </Popover>
      ) : null}
    </div>
  );
}

/** Read-only fallback: no add button, no row menu. */
function ReadOnlyRoots() {
  const roots = useApp((s) => s.roots);
  const activeRootId = useApp((s) => s.activeRootId);
  const selectRoot = useApp((s) => s.selectRoot);

  return (
    <div className="shrink-0 px-1.5 pb-1">
      <SectionHeader
        title="文件夹"
        count={roots.length}
        action={<span className="text-[9.5px] text-ink4">只读</span>}
      />
      <div className="max-h-[186px] overflow-y-auto pr-0.5">
        {roots.map((root) => {
          const selected = root.id === activeRootId;
          return (
            <div
              key={root.id}
              role="button"
              tabIndex={0}
              onClick={() => void selectRoot(root.id)}
              className={cx(
                'relative flex h-[34px] select-none items-center rounded-[6px] pl-2.5 pr-2',
                selected ? 'bg-hover' : 'hover:bg-hover',
              )}
            >
              {selected ? (
                <span className="absolute bottom-[3px] left-0 top-[3px] w-[2px] rounded-full bg-accent" />
              ) : null}
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13px] leading-[15px] text-ink2">{root.alias}</div>
                <div className="truncate font-mono text-[10px] leading-[13px] text-ink4">
                  {root.path}
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * Version zone (slot: sidebar.versions)
 * ------------------------------------------------------------------ */

function VersionRow({ version }: { version: VersionSummary }) {
  const activeVersionId = useApp((s) => s.activeVersionId);
  const selectVersion = useApp((s) => s.selectVersion);
  const rootId = useApp((s) => s.activeRootId);
  const showToast = useUI((s) => s.showToast);
  const selected = version.id === activeVersionId;
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [dialog, setDialog] = useState<'delete' | 'loader' | null>(null);

  return (
    <>
      <div
        role="button"
        tabIndex={0}
        onClick={() => void selectVersion(version.id)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            void selectVersion(version.id);
          }
        }}
        title={version.gameVersion ? `${version.id} · Minecraft ${version.gameVersion}` : version.id}
        className={cx(
          'group relative flex h-[28px] w-full cursor-default select-none items-center gap-2 rounded-[5px] pl-2.5 pr-1 text-left transition-colors',
          selected ? 'bg-hover' : 'hover:bg-hover',
        )}
      >
        {selected ? (
          <span className="absolute bottom-[3px] left-0 top-[3px] w-[2px] rounded-full bg-accent" />
        ) : null}
        <img
          src={LOADER_ICONS[version.loader as keyof typeof LOADER_ICONS] ?? VERSION_ICONS.release}
          alt=""
          draggable={false}
          className="h-[18px] w-[18px] shrink-0 rounded-none object-contain [image-rendering:auto]"
        />
        <span
          className={cx(
            'min-w-0 flex-1 truncate text-[12px]',
            selected ? 'text-ink' : 'text-ink2 group-hover:text-ink',
          )}
        >
          {version.id}
        </span>
        {version.loaderVersion ? (
          <span className="shrink-0 font-mono text-[9.5px] leading-none text-ink4 group-hover:hidden">
            {version.loaderVersion}
          </span>
        ) : null}
        <IconButton
          label="版本操作"
          size={20}
          className={cx(
            'shrink-0 opacity-0 group-hover:opacity-100 focus-visible:opacity-100',
            menu && 'opacity-100',
          )}
          onClick={(e) => {
            e.stopPropagation();
            const rect = e.currentTarget.getBoundingClientRect();
            setMenu({ x: rect.right, y: rect.bottom });
          }}
        >
          <MoreHorizontal size={12} />
        </IconButton>
      </div>

      {menu ? (
        <Popover x={menu.x} y={menu.y} onClose={() => setMenu(null)}>
          <MenuLabel>{version.id}</MenuLabel>
          <MenuItem
            onClick={() => {
              setMenu(null);
              if (version.loader === 'vanilla') {
                showToast('这个版本没有安装模组加载器');
                return;
              }
              setDialog('loader');
            }}
          >
            更新加载器…
          </MenuItem>
          <MenuItem
            onClick={() => {
              setMenu(null);
              void selectVersion(version.id);
            }}
          >
            打开版本设置
          </MenuItem>
          <MenuDivider />
          <MenuItem
            danger
            onClick={() => {
              setMenu(null);
              setDialog('delete');
            }}
          >
            删除版本
          </MenuItem>
        </Popover>
      ) : null}

      <VersionDeleteDialog
        rootId={rootId}
        version={dialog === 'delete' ? version : null}
        onClose={() => setDialog(null)}
      />
      <LoaderUpdateDialog
        rootId={rootId}
        version={dialog === 'loader' ? version : null}
        onClose={() => setDialog(null)}
      />
    </>
  );
}

function useGroupedVersions(versions: VersionSummary[]) {
  return LOADER_GROUPS.map((group) => ({
    ...group,
    items: versions.filter((v) => v.loader === group.kind),
  })).filter((group) => group.items.length > 0);
}

export function VersionsZone() {
  const versions = useApp((s) => s.versions);
  const scanning = useApp((s) => s.scanning);
  const refreshVersions = useApp((s) => s.refreshVersions);
  const activeRoot = useApp((s) => s.roots.find((r) => r.id === s.activeRootId) ?? null);
  const [query, setQuery] = useState('');
  const [collapsed, setCollapsed] = useState<Set<LoaderKind>>(new Set(['unrecognized']));
  const searchRef = useRef<HTMLInputElement>(null);
  const focusNonce = useUI((s) => s.focusVersionsNonce);

  useEffect(() => {
    if (focusNonce > 0) searchRef.current?.focus();
  }, [focusNonce]);

  const filtered = query.trim()
    ? versions.filter((v) => v.id.toLowerCase().includes(query.trim().toLowerCase()))
    : versions;
  const groups = useGroupedVersions(filtered);

  const toggleGroup = (kind: LoaderKind) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <SectionHeader
        title="版本"
        count={versions.length}
        action={
          <IconButton
            label="重新读取版本"
            size={20}
            disabled={scanning || !activeRoot}
            onClick={() => void refreshVersions()}
          >
            <RefreshCw size={11} className={scanning ? 'animate-spin' : undefined} />
          </IconButton>
        }
      />
      <div className="px-3 pb-1.5">
        <div className="relative">
          <Search
            size={11}
            className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-ink4"
          />
          <Input
            ref={searchRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索此文件夹的版本"
            className="h-[24px] rounded-[5px] pl-6 pr-2 text-[11.5px]"
            aria-label="搜索版本"
          />
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-2">
        {versions.length === 0 ? (
          <div className="px-2 py-3">
            <p className="text-[11.5px] text-ink3">此目录暂无版本</p>
            <p className="mt-1 text-[10.5px] leading-[1.5] text-ink4">
              {activeRoot?.kind === 'empty_folder'
                ? '空目录 · 安装一个版本后会出现在这里'
                : '扫描完成后版本会分组显示在这里'}
            </p>
          </div>
        ) : null}

        {filtered.length === 0 && versions.length > 0 ? (
          <p className="px-2 py-3 text-[11.5px] text-ink4">没有匹配「{query.trim()}」的版本</p>
        ) : null}

        {groups.map((group) => {
          const isCollapsed = collapsed.has(group.kind);
          return (
            <div key={group.kind} className="mb-0.5">
              <button
                type="button"
                onClick={() => toggleGroup(group.kind)}
                className="flex h-[22px] w-full select-none items-center gap-1 rounded-[4px] px-1.5 text-ink4 transition-colors hover:bg-hover hover:text-ink3"
              >
                {isCollapsed ? <ChevronRight size={11} /> : <ChevronDown size={11} />}
                <span className="text-[10.5px] font-medium tracking-[0.02em]">{group.label}</span>
                <span className="ml-auto font-mono text-[9.5px] leading-none text-ink4">
                  {group.items.length}
                </span>
              </button>
              <Collapse open={!isCollapsed}>
                <div className="space-y-[1px]">
                  {group.items.map((version) => (
                    <VersionRow key={version.id} version={version} />
                  ))}
                </div>
              </Collapse>
            </div>
          );
        })}

        {scanning ? <p className="px-2 py-2 text-[10.5px] text-ink4">扫描中…</p> : null}
      </div>
    </div>
  );
}

/** Read-only fallback: same data, no search. */
function ReadOnlyVersions() {
  const versions = useApp((s) => s.versions);
  const groups = useGroupedVersions(versions);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <SectionHeader
        title="版本"
        count={versions.length}
        action={<span className="text-[9.5px] text-ink4">只读</span>}
      />
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-2 pt-1">
        {versions.length === 0 ? (
          <p className="px-2 py-2 text-[11.5px] text-ink4">此目录暂无版本</p>
        ) : null}
        {groups.map((group) => (
          <div key={group.kind} className="mb-1">
            <div className="flex h-[22px] items-center gap-1 px-1.5 text-ink4">
              <span className="text-[10.5px] font-medium">{group.label}</span>
              <span className="ml-auto font-mono text-[9.5px]">{group.items.length}</span>
            </div>
            <div className="space-y-[1px]">
              {group.items.map((version) => (
                <VersionRow key={version.id} version={version} />
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * Settings footer (slot: sidebar.settings)
 * ------------------------------------------------------------------ */

export function SettingsFooter() {
  const openSettings = useUI((s) => s.openSettings);
  const settingsOpen = useUI((s) => s.settingsOpen);
  return (
    <div className="mt-auto border-t border-line py-1">
      <button
        type="button"
        onClick={() => openSettings('app')}
        className={cx(
          'flex h-[32px] w-full select-none items-center gap-2 px-3 text-[12px] transition-colors',
          settingsOpen ? 'bg-hover text-ink' : 'text-ink3 hover:bg-hover hover:text-ink',
        )}
      >
        <Settings size={14} className="text-ink4" />
        <span>设置</span>
        <span className="kbd ml-auto">Ctrl ,</span>
      </button>
    </div>
  );
}

/* ------------------------------------------------------------------ *
 * Rail shell
 * ------------------------------------------------------------------ */

export function Rail() {
  const collapsed = useUI((s) => s.railCollapsed);
  const toggleRail = useUI((s) => s.toggleRail);
  const openSettings = useUI((s) => s.openSettings);
  const focusVersions = useUI((s) => s.focusVersions);
  const railWidth = usePrefs((s) => s.railWidth);
  const versionCount = useApp((s) => s.versions.length);

  if (collapsed) {
    return (
      <aside
        style={{ width: 56 }}
        className="anim-fade-in flex h-full shrink-0 flex-col items-center gap-1 border-r border-line bg-rail py-2.5"
      >
        <button
          type="button"
          onClick={toggleRail}
          title="展开侧边栏"
          className="flex h-[30px] w-[30px] items-center justify-center rounded-[7px] transition-colors hover:bg-hover"
        >
          <BrandMark size={22} />
        </button>
        <div className="my-1 h-px w-6 bg-line" />
        <IconButton label="文件夹" size={32} onClick={toggleRail}>
          <Folder size={15} />
        </IconButton>
        <IconButton label={`版本 ${versionCount}`} size={32} onClick={focusVersions}>
          <Layers size={15} />
        </IconButton>
        <IconButton label="设置" size={32} className="mt-auto" onClick={() => openSettings('app')}>
          <Settings size={15} />
        </IconButton>
        <IconButton label="展开侧边栏" size={32} onClick={toggleRail}>
          <PanelLeftOpen size={15} />
        </IconButton>
      </aside>
    );
  }

  return (
    <aside
      style={{ width: railWidth }}
      className="anim-fade-in flex h-full shrink-0 flex-col border-r border-line bg-rail"
    >
      <div className="flex h-[44px] shrink-0 items-center gap-2 px-3">
        <BrandMark />
        <span className="text-[13px] font-semibold tracking-[0.01em] text-ink">Kaleid</span>
        <span className="mt-[1px] font-mono text-[9.5px] text-ink4">v0.1</span>
        <IconButton label="收起侧边栏" size={24} className="ml-auto" onClick={toggleRail}>
          <PanelLeftClose size={13} />
        </IconButton>
      </div>

      <div className="px-3 pb-2.5">
        <Slot name="sidebar.header.action" fallback={ReadOnlyAction} />
      </div>

      <div className="mx-3 h-px bg-line" />
      <Slot name="sidebar.roots" fallback={ReadOnlyRoots} />
      <div className="mx-3 h-px bg-line" />
      <Slot name="sidebar.versions" fallback={ReadOnlyVersions} />
      <Slot name="sidebar.settings" fallback={SettingsFooter} />
    </aside>
  );
}
