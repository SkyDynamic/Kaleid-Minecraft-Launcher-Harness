/**
 * 启动被拦下的原因弹窗。
 *
 * 三种来源共用它：登录失效（令牌取不到/被微软拒绝）、缺 Java、版本文件不完整。
 * 设计上只有一条原则：**原因必须原样给出**。笼统一句"启动失败"只会让人反复点按钮。
 *
 * 登录相关的拦截额外给一个「去登录」入口，直接把用户送到账号卡那里。
 */
import { AlertTriangle, LogIn, Settings2 } from 'lucide-react';
import { Dialog } from './Dialog';
import { Button } from '../primitives';
import type { LaunchBlock, LaunchBlockKind } from '../bridge/types';
import { useUI } from '../store/ui';

const KIND_LABEL: Record<LaunchBlockKind, string> = {
  'no-account': '没有账号',
  auth: '登录失效',
  java: '找不到 Java',
  files: '版本文件不完整',
  version: '版本不可用',
};

const KIND_HINT: Record<LaunchBlockKind, string> = {
  'no-account': '启动器需要一个账号才能进游戏，离线账号也算。',
  auth: '微软的登录令牌已经不能用了（过期、被吊销，或者刷新时被拒绝）。重新登录一次就好。',
  java: '这个版本要求的 Java 没找到。可以在「设置 → 游戏全局设置」里指定 Java 路径。',
  files: '版本的关键文件缺失（jar、依赖库或资源索引），需要重新下载或修复这个版本。',
  version: '这个版本的版本 JSON 读不出来，通常是被改坏或下载不完整。',
};

/** 这条原因该给什么快捷动作。与 Rust 侧 `LaunchBlockKind::wants_relogin` 保持一致。 */
function wantsRelogin(kind: LaunchBlockKind): boolean {
  return kind === 'no-account' || kind === 'auth';
}

export function LaunchBlockedDialog({
  blocks,
  onClose,
}: {
  blocks: LaunchBlock[] | null;
  onClose: () => void;
}) {
  const setTab = useUI((s) => s.setTab);
  const openSettings = useUI((s) => s.openSettings);

  const list = blocks ?? [];
  const kinds = new Set(list.map((block) => block.kind));
  const anyRelogin = [...kinds].some(wantsRelogin);
  const anyJava = kinds.has('java') || kinds.has('version');

  return (
    <Dialog
      open={list.length > 0}
      onClose={onClose}
      title="游戏没有启动"
      subtitle="启动前检查没通过，已经拦住了这次启动"
      width={520}
      footer={
        <div className="flex items-center justify-end gap-2">
          <Button variant="secondary" onClick={onClose}>
            知道了
          </Button>
          {anyJava ? (
            <Button
              variant="secondary"
              icon={<Settings2 size={11} />}
              onClick={() => {
                onClose();
                openSettings('game');
              }}
            >
              打开游戏设置
            </Button>
          ) : null}
          {anyRelogin ? (
            <Button
              variant="primary"
              icon={<LogIn size={11} />}
              onClick={() => {
                onClose();
                // 登录卡在「概览」的第一屏
                setTab('overview');
              }}
            >
              去登录
            </Button>
          ) : null}
        </div>
      }
    >
      <div className="flex flex-col gap-2">
        {list.map((block, index) => (
          <div
            key={`${block.kind}-${index}`}
            className="rounded-[6px] border border-line bg-layer2 px-3 py-2"
          >
            <div className="flex items-center gap-1.5">
              <AlertTriangle size={11} className="shrink-0 text-amber" />
              <span className="text-[11.5px] font-medium text-ink">{KIND_LABEL[block.kind]}</span>
            </div>
            {/* 原因原文：宿主给什么就显示什么，不再二次解释 */}
            <p className="mt-1 whitespace-pre-wrap break-words text-[11.5px] leading-[17px] text-ink2">
              {block.message}
            </p>
            <p className="mt-1 text-[10.5px] leading-[15px] text-ink4">{KIND_HINT[block.kind]}</p>
          </div>
        ))}
      </div>
    </Dialog>
  );
}
