import { useCallback, useEffect, useState } from 'react';
import { Plus, TriangleAlert } from 'lucide-react';
import { getBridge } from '../bridge';
import type { Account, AccountKind, MsaAccountState, MsaConfig } from '../bridge/types';
import { Button, Card, Label, PillTabs, TextLink } from '../primitives';
import { McHead } from '../components/McHead';
import { Dot } from '../components/Dot';
import { InstancePanel } from '../components/InstancePanel';
import { MsaLoginDialog } from '../components/MsaLoginDialog';
import { Slot } from '../shell/slots';
import { useApp, usePluginEnabled } from '../store/app';
import { usePickedJava } from '../store/java';
import { useSettings } from '../store/settings';
import { useUI } from '../store/ui';
import { cx, formatGb } from '../lib/util';

/**
 * 离线名允许汉字：原版客户端只拿它算 `OfflinePlayer:<名字>` 的 uuid，本地不校验。
 * 但服务器侧（正版校验、白名单、部分聊天组件）多半不认，所以输入框下面要挂
 * 一条黄字警告，别让用户装完才发现进不去服。
 */
const OFFLINE_NAME_RE = /^[A-Za-z0-9_\u4e00-\u9fa5]{1,16}$/;

/** 名字里是否含非 ASCII 字符（也就是"用了中文"）。 */
function hasNonAscii(name: string): boolean {
  return /[^\x00-\x7f]/.test(name);
}

function InfoCard({
  label,
  value,
  onGoto,
}: {
  label: string;
  value: string;
  onGoto: () => void;
}) {
  return (
    <Card className="flex min-h-[112px] flex-col items-center justify-center gap-1.5 px-4 py-5 text-center">
      <Label>{label}</Label>
      <div className="text-[17px] font-semibold leading-[22px] tracking-[-0.01em] text-ink">
        {value}
      </div>
      <TextLink onClick={onGoto}>前往设置 ›</TextLink>
    </Card>
  );
}

export function Overview() {
  const memoryMb = useSettings((s) => s.effective?.memoryMb ?? s.global?.memoryMb ?? 4096);
  const gotoRow = useUI((s) => s.gotoSettingRow);
  const version = useApp((s) => s.versions.find((v) => v.id === s.activeVersionId) ?? null);
  const javaEnabled = usePluginEnabled('java-runtime');
  const java = usePickedJava(version?.javaMajor ?? null, javaEnabled);

  const javaValue = !javaEnabled
    ? '插件已停用'
    : java.loading
      ? '检测中…'
      : java.runtime
        ? java.runtime.name
        : '未找到';

  return (
    <div className="p-3">
      <div className="grid grid-cols-2 gap-[10px]">
        <InfoCard label="JAVA 运行时" value={javaValue} onGoto={() => gotoRow('java')} />
        <InfoCard label="内存分配" value={formatGb(memoryMb)} onGoto={() => gotoRow('memory')} />
        <div className="col-span-2">
          <Slot name="overview.login" fallback={LoginCard} />
        </div>
        {/* 已启动实例的监控面板：多开时这里是唯一的统一入口。 */}
        <div className="col-span-2">
          <InstancePanel />
        </div>
        {/* 插件面板是**追加**的：这里只往下加一块只读说明，不替换任何内置界面。 */}
        <PluginNotices />
      </div>
    </div>
  );
}

interface PluginNotice {
  plugin: string;
  slot: string;
  title: string;
  text: string;
}

/** 第三方清单通过 `bind-slot` 往 `overview.notices` 加只读说明。 */
function PluginNotices() {
  const [notices, setNotices] = useState<PluginNotice[]>([]);

  useEffect(() => {
    let cancelled = false;
    getBridge()
      .invoke<PluginNotice[]>('plugins_slots')
      .then((rows) => {
        if (!cancelled) setNotices(rows.filter((row) => row.slot === 'overview.notices'));
      })
      .catch(() => {
        if (!cancelled) setNotices([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (notices.length === 0) return null;
  return (
    <>
      {notices.map((notice) => (
        <Card key={`${notice.plugin}:${notice.title}`} className="col-span-2 px-4 py-3">
          <Label>{notice.title}</Label>
          <p className="mt-1 text-[12px] leading-[1.6] text-ink3">{notice.text}</p>
        </Card>
      ))}
    </>
  );
}

/* ------------------------------------------------------------------ *
 * Login card (slot: overview.login)
 * ------------------------------------------------------------------ */

export function LoginCard() {
  const accounts = useApp((s) => s.accounts);
  const active = useApp((s) => s.activeAccount);
  const activeKind: AccountKind = active?.kind ?? 'msa';
  const [tab, setTab] = useState<AccountKind>(activeKind);

  // The active account decides which tab is showing.
  useEffect(() => {
    setTab(activeKind);
  }, [activeKind]);

  return (
    <Card className="px-4 py-4">
      <Label>登录</Label>

      <div className="mt-3 flex justify-center">
        <PillTabs
          items={[
            { id: 'msa', label: '正版账号' },
            { id: 'offline', label: '离线登录' },
          ]}
          value={tab}
          onChange={(id) => setTab(id as AccountKind)}
        />
      </div>

      <div className="mt-4">
        {tab === 'msa' ? (
          <MsaPanel accounts={accounts} active={active} />
        ) : (
          <OfflinePanel accounts={accounts} />
        )}
      </div>
    </Card>
  );
}

function MsaPanel({ accounts, active }: { accounts: Account[]; active: Account | null }) {
  const setActiveAccount = useApp((s) => s.setActiveAccount);
  const removeAccount = useApp((s) => s.removeAccount);
  const refreshAccounts = useApp((s) => s.refreshAccounts);
  const showToast = useUI((s) => s.showToast);

  const [config, setConfig] = useState<MsaConfig | null>(null);
  const [states, setStates] = useState<MsaAccountState[]>([]);
  const [loginOpen, setLoginOpen] = useState(false);

  // client_id 是编译期注入的：先问后端"配没配"，没配就把按钮变成一条说明，
  // 而不是让用户点进去撞一个网络错误。命令本身失败（服务没起来）也照实说出来。
  useEffect(() => {
    let cancelled = false;
    void getBridge()
      .invoke<MsaConfig>('msa_config', {})
      .then((value) => !cancelled && setConfig(value))
      .catch((e: unknown) =>
        !cancelled &&
        setConfig({
          configured: false,
          hint: e instanceof Error ? e.message : String(e),
        }),
      );
    return () => {
      cancelled = true;
    };
  }, []);

  const loadStates = useCallback(() => {
    void getBridge()
      .invoke<MsaAccountState[]>('msa_states', {})
      .then(setStates)
      .catch(() => setStates([]));
  }, []);

  useEffect(() => {
    loadStates();
  }, [loadStates, accounts]);

  const hero = active?.kind === 'msa' ? active : null;
  const others = accounts.filter((acc) => acc.kind === 'msa' && acc.id !== hero?.id);
  const stateOf = (id: string) => states.find((s) => s.accountId === id) ?? null;

  return (
    <div>
      <div className="flex flex-col items-center gap-2">
        {/* key 跟着账号走：换账号时头像淡入，而不是"啪"地换一张脸。 */}
        <div key={hero?.id ?? 'guest'} className="anim-fade-in">
          <McHead
            name={hero?.name ?? '未登录'}
            uuid={hero?.uuid}
            skinUrl={hero?.skinUrl}
            skinModel={hero?.skinModel}
            size={52}
          />
        </div>
        <div className="text-center">
          <div className="text-[15px] font-semibold leading-[19px] text-ink">
            {hero?.name ?? '未登录'}
          </div>
          <div className="mt-[3px] text-[11px] leading-[14px] text-ink4">
            {hero && stateOf(hero.id)?.usable === false
              ? '正版账号 · 登录已过期'
              : '正版账号'}
          </div>
        </div>
      </div>

      <div className="my-3.5 h-px w-full bg-line" />

      {others.length > 0 ? (
        <div className="mx-auto flex max-w-[340px] flex-col">
          {others.map((account) => (
            <div
              key={account.id}
              className="group flex h-[34px] items-center gap-2.5 rounded-[6px] px-1 hover:bg-hover"
            >
              <McHead
                name={account.name}
                uuid={account.uuid}
                skinUrl={account.skinUrl}
                skinModel={account.skinModel}
                size={24}
              />
              <span className="min-w-0 flex-1 truncate text-[12.5px] text-ink2">
                {account.name}
              </span>
              <TextLink onClick={() => void setActiveAccount(account.id)}>切换</TextLink>
              <TextLink
                tone="muted"
                className="hover:text-red"
                onClick={() => void removeAccount(account.id)}
              >
                移除
              </TextLink>
            </div>
          ))}
        </div>
      ) : null}

      <div className="mt-3 flex flex-col items-center gap-1.5">
        <Button
          variant="secondary"
          size="sm"
          icon={<Plus size={12} />}
          disabled={config?.configured === false}
          title={config?.configured === false ? (config.hint ?? undefined) : undefined}
          onClick={() => {
            if (config?.configured === false) {
              showToast(config.hint ?? '还没配置 client_id，见 build.env');
              return;
            }
            setLoginOpen(true);
          }}
        >
          添加正版账号
        </Button>
        {config?.configured === false ? (
          <p className="m-0 max-w-[320px] text-center text-[10.5px] leading-[14px] text-ink4">
            还没配置微软 client_id（build.env 的 KALEID_MSA_CLIENT_ID），配好重新编译即可登录。
          </p>
        ) : null}
        {hero && stateOf(hero.id)?.usable === false ? (
          <p className="m-0 max-w-[320px] text-center text-[10.5px] leading-[14px] text-ink4">
            访问令牌已过期，启动时会自动用刷新令牌续期；续不上就重新添加一次账号。
          </p>
        ) : null}
      </div>

      <MsaLoginDialog
        open={loginOpen}
        config={config}
        onClose={() => setLoginOpen(false)}
        onDone={(name) => {
          setLoginOpen(false);
          showToast(`已添加正版账号 ${name}`);
          void refreshAccounts();
          loadStates();
        }}
      />
    </div>
  );
}

function offlineReason(value: string, current: string): string {
  if (value.length === 0) return `名称不能为空；当前仍是 ${current}`;
  if ([...value].length > 16) return `最多 16 位字母、数字、下划线或汉字；当前仍是 ${current}`;
  return `只能用 1–16 位字母、数字、下划线或汉字；当前仍是 ${current}`;
}

function OfflinePanel({ accounts }: { accounts: Account[] }) {
  const offline = accounts.find((acc) => acc.kind === 'offline') ?? null;
  const applyOffline = useApp((s) => s.applyOffline);

  const [value, setValue] = useState(offline?.name ?? '');
  const [applied, setApplied] = useState(offline?.name ?? '');
  const [pending, setPending] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);

  const valid = OFFLINE_NAME_RE.test(value);
  const current = applied || offline?.name || '未设置';

  // Live apply: debounce 300 ms, only valid names ever reach the bridge.
  useEffect(() => {
    if (value === applied) {
      setPending(false);
      return;
    }
    setPending(true);
    setServerError(null);
    const timer = setTimeout(() => {
      if (!OFFLINE_NAME_RE.test(value)) {
        setPending(false);
        return;
      }
      void applyOffline(value)
        .then((account) => {
          setApplied(account.name);
          setServerError(null);
        })
        .catch((e: unknown) => {
          setServerError(e instanceof Error ? e.message : String(e));
        })
        .finally(() => setPending(false));
    }, 300);
    return () => clearTimeout(timer);
  }, [value, applied, applyOffline]);

  const errorText = valid ? serverError : offlineReason(value, current);

  return (
    <div>
      <div className="flex flex-col items-center gap-2">
        <McHead
          name={offline?.name ?? 'Offline'}
          uuid={offline?.uuid}
          skinUrl={offline?.skinUrl}
          skinModel={offline?.skinModel}
          size={52}
        />
        <div className="text-center">
          <div className="text-[15px] font-semibold leading-[19px] text-ink">
            {offline?.name ?? '未设置'}
          </div>
          <div className="mt-[3px] text-[11px] leading-[14px] text-ink4">离线账号</div>
        </div>
      </div>

      <div className="mx-auto mt-4 max-w-[320px]">
        <input
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="输入游戏内名称"
          spellCheck={false}
          autoComplete="off"
          aria-label="离线游戏名称"
          aria-invalid={!valid}
          onKeyDown={(e) => {
            if (e.key === 'Enter') e.preventDefault();
          }}
          className={cx(
            'h-[32px] w-full rounded-[7px] border bg-field px-3 text-center text-[13px] text-ink transition-colors',
            'placeholder:text-ink4 focus:outline-none',
            valid
              ? 'border-line hover:border-line-strong focus:border-accent/70 focus:ring-1 focus:ring-accent/25'
              : 'border-red/70 ring-1 ring-red/40',
          )}
        />

        <div className="mt-2 flex min-h-[16px] items-center justify-center gap-1.5 text-[10.5px] leading-[14px]">
          {errorText ? (
            <span className="text-red">{errorText}</span>
          ) : pending ? (
            <span className="text-ink4">停笔 300 ms 后自动生效</span>
          ) : (
            <>
              <Dot tone="green" size={5} />
              <span className="text-ink4">已生效 · 输入即保存</span>
            </>
          )}
        </div>

        {valid && hasNonAscii(value) ? (
          <div className="mt-1.5 flex items-start gap-1.5 rounded-[7px] border border-amber/30 bg-amber/10 px-2.5 py-1.5 text-left text-[10.5px] leading-[15px] text-amber">
            <TriangleAlert size={12} className="mt-[1.5px] shrink-0" />
            <span>
              原则上Minecraft任何版本都不接受使用中文ID，若出现问题后果自负
            </span>
          </div>
        ) : null}
      </div>
    </div>
  );
}
