/**
 * Java 运行时的小工具。
 *
 * 只做一件事：界面想知道"这个版本会用哪个 Java"时，别自己猜——问宿主
 * `java_pick`，它按版本 JSON 的要求挑，并告诉我们这个 Java 是哪来的。
 */
import { useEffect, useState } from 'react';
import { getBridge } from '../bridge';
import type { JavaRuntime } from '../bridge/types';

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

export interface PickedJava {
  runtime: JavaRuntime | null;
  error: string | null;
  loading: boolean;
}

/**
 * 自动匹配的结果（不传 `explicit`，所以走宿主的扫描缓存）。
 *
 * `major` 变一次就问一次；`enabled = false` 时完全不请求（例如插件停了）。
 */
export function usePickedJava(major: number | null, enabled = true): PickedJava {
  const [state, setState] = useState<PickedJava>({
    runtime: null,
    error: null,
    loading: enabled,
  });

  useEffect(() => {
    if (!enabled) {
      setState({ runtime: null, error: null, loading: false });
      return;
    }
    let cancelled = false;
    setState((prev) => ({ ...prev, loading: true }));
    void getBridge()
      .invoke<JavaRuntime | null>('java_pick', { major })
      .then((runtime) => {
        if (!cancelled) setState({ runtime, error: null, loading: false });
      })
      .catch((e: unknown) => {
        if (!cancelled) setState({ runtime: null, error: errText(e), loading: false });
      });
    return () => {
      cancelled = true;
    };
  }, [major, enabled]);

  return state;
}
