import { useCallback, useEffect, useRef, useState } from 'react';
import { Copy, ExternalLink, Loader2, TriangleAlert } from 'lucide-react';
import { getBridge, openExternal } from '../bridge';
import type { DeviceCodeInfo, MsaConfig, MsaPoll } from '../bridge/types';
import { Button } from '../primitives';
import { Dialog } from './Dialog';
import { useUI } from '../store/ui';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** 轮询节奏：固定 10 秒一次（后端也按这个下限兜底，两边一致）。 */
const POLL_SECS = 10;
const pollDelay = (serverInterval: number) =>
  Math.max(POLL_SECS, serverInterval || POLL_SECS) * 1000;

function clockLeft(until: number, now: number): string {
  const left = Math.max(0, until - now);
  const m = Math.floor(left / 60);
  const s = left % 60;
  return `${m}:${s.toString().padStart(2, '0')}`;
}

/**
 * 微软账号登录弹窗（OAuth2 设备码流程）。
 *
 * 为什么是"弹窗里显示一串码"，而不是"内嵌一个微软登录页"：启动器是公共客户端，
 * 设备码流程不需要回调端口、不需要 client_secret，也就不用把一个第三方登录页
 * 塞进 WebView——用户在**自己熟悉的浏览器**里登录，还能用手机上已经登录的账号。
 *
 * 轮询完全由这个组件驱动：`start` 拿到码 → 按 `interval` 调 `poll` →
 * `pending` 就等下一轮。关掉弹窗会立刻 `cancel` 并停表，不在后台空转。
 */
export function MsaLoginDialog({
  open,
  config,
  onClose,
  onDone,
}: {
  open: boolean;
  config: MsaConfig | null;
  onClose: () => void;
  /** 登录成功（账号已经登记好）。 */
  onDone: (name: string) => void;
}) {
  const showToast = useUI((s) => s.showToast);

  const [code, setCode] = useState<DeviceCodeInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  /** 浏览器是否已经被我们自动打开了（打不开就退回"点按钮"的老路）。 */
  const [autoOpened, setAutoOpened] = useState(false);

  // 轮询定时器与"这次弹窗是否还有效"的标记
  const timer = useRef<number | null>(null);
  const alive = useRef(false);

  const stop = useCallback(() => {
    alive.current = false;
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  }, []);

  // 关窗即取消：后端只清设备码，不动已经登录的账号
  useEffect(() => {
    if (!open) {
      stop();
      setCode(null);
      setError(null);
      setAutoOpened(false);
      return;
    }
    return stop;
  }, [open, stop]);

  // 本地倒计时：设备码 15 分钟内有效，用户需要知道还剩多久
  useEffect(() => {
    if (!open || !code) return;
    const id = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => window.clearInterval(id);
  }, [open, code]);

  const pollOnce = useCallback(
    async (delayMs: number) => {
      stop();
      alive.current = true;
      timer.current = window.setTimeout(() => {
        void (async () => {
          if (!alive.current) return;
          try {
            const result = await getBridge().invoke<MsaPoll>('msa_poll', {});
            if (!alive.current) return;
            if (result.state === 'done') {
              stop();
              onDone(result.account.name);
              return;
            }
            void pollOnce(pollDelay(result.interval));
          } catch (e) {
            if (!alive.current) return;
            stop();
            setError(errText(e));
          }
        })();
      }, delayMs);
    },
    [onDone, stop],
  );

  const start = useCallback(async () => {
    setStarting(true);
    setError(null);
    try {
      const info = await getBridge().invoke<DeviceCodeInfo>('msa_start', {});
      setCode(info);
      setNow(Math.floor(Date.now() / 1000));
      // 拿到码就顺手把浏览器打开：用户要做的事只剩"输入这 8 位"，少一次点击。
      // 打不开也没关系（无默认浏览器/被拦截），下面的「打开登录页」按钮还在，
      // 所以这里绝不能让异常打断轮询。
      const opened = await openExternal(info.verificationUri).catch(() => false);
      setAutoOpened(opened);
      void pollOnce(pollDelay(info.interval));
    } catch (e) {
      setError(errText(e));
    } finally {
      setStarting(false);
    }
  }, [pollOnce]);

  const cancel = () => {
    stop();
    void getBridge()
      .invoke<null>('msa_cancel', {})
      .catch(() => {
        /* 取消失败不该拦住关窗：下次 start 会覆盖掉旧设备码 */
      });
    onClose();
  };

  const copy = async () => {
    if (!code) return;
    try {
      await navigator.clipboard.writeText(code.userCode);
      showToast('已复制代码，粘到浏览器的输入框里');
    } catch {
      showToast(`复制失败，请手动输入：${code.userCode}`);
    }
  };

  const configured = config?.configured !== false;
  const expired = code !== null && now >= code.expiresAt;

  return (
    <Dialog
      open={open}
      onClose={cancel}
      title="添加正版账号"
      subtitle="用浏览器完成微软账号登录，登录器不会拿到你的密码"
      width={460}
      footer={
        <div className="flex items-center justify-between">
          <Button
            variant="ghost"
            size="sm"
            icon={<ExternalLink size={11} />}
            disabled={!code}
            onClick={() => {
              if (code) void openExternal(code.verificationUri);
            }}
          >
            打开登录页
          </Button>
          <div className="flex items-center gap-2">
            {error || expired ? (
              <Button variant="secondary" onClick={() => void start()} disabled={starting}>
                重新获取代码
              </Button>
            ) : null}
            <Button variant="primary" onClick={cancel}>
              取消
            </Button>
          </div>
        </div>
      }
    >
      <div className="p-4 text-[12px] leading-[20px] text-ink2">
        {!configured ? (
          <div className="flex gap-2 rounded-[6px] border border-line bg-layer2 px-2.5 py-2">
            <TriangleAlert size={13} className="mt-[3px] shrink-0 text-ink3" />
            <span className="text-ink3">
              {config?.hint ?? '还没配置 client_id：请在 build.env 里填 KALEID_MSA_CLIENT_ID 后重新编译。'}
            </span>
          </div>
        ) : null}

        {configured && !code && !error ? (
          <div className="flex flex-col items-center gap-3 py-2">
            <p className="m-0 text-ink3">点下面的按钮向微软申请一个设备码。</p>
            <Button variant="primary" onClick={() => void start()} disabled={starting}>
              {starting ? '正在申请…' : '开始登录'}
            </Button>
          </div>
        ) : null}

        {error ? (
          <div className="rounded-[6px] border border-line bg-layer2 px-2.5 py-2">
            <p className="m-0 whitespace-pre-wrap break-all text-ink3">{error}</p>
          </div>
        ) : null}

        {code ? (
          <div className={error ? 'mt-3' : ''}>
            <ol className="m-0 list-decimal pl-5 text-ink3">
              <li>
                {autoOpened ? '已经为你打开 ' : '打开 '}
                <button
                  type="button"
                  className="m-0 cursor-pointer border-0 bg-transparent p-0 font-mono text-[11.5px] text-accent underline-offset-2 hover:underline"
                  onClick={() => void openExternal(code.verificationUri)}
                >
                  {code.verificationUri.replace(/^https?:\/\//, '')}
                </button>
                {autoOpened ? '（没弹出来就点一下）' : ''}
              </li>
              <li>输入下面这串代码，然后用你的微软账号登录。</li>
              <li>在这里等下：登录成功后会自己加进来。</li>
            </ol>

            <div className="mt-3 flex items-center justify-center gap-2 rounded-[8px] border border-line bg-code px-3 py-3">
              <span className="select-all font-mono text-[20px] font-semibold tracking-[0.18em] text-ink">
                {code.userCode}
              </span>
              <Button
                variant="ghost"
                size="sm"
                icon={<Copy size={12} />}
                onClick={() => void copy()}
              >
                复制
              </Button>
            </div>

            <div className="mt-2 flex items-center justify-center gap-1.5 text-[11px] text-ink4">
              {expired ? (
                <span className="text-ink3">代码已过期，重新获取一个。</span>
              ) : (
                <>
                  <Loader2 size={11} className="animate-spin" />
                  <span>
                    正在等待你在浏览器里完成登录… 还剩{' '}
                    <span className="font-mono">{clockLeft(code.expiresAt, now)}</span>
                  </span>
                </>
              )}
            </div>
          </div>
        ) : null}
      </div>
    </Dialog>
  );
}

