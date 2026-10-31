import { Dialog } from '../components/Dialog';
import { useUI, type SettingsSection } from '../store/ui';
import { cx } from '../lib/util';
import { AppSection } from './settings/AppSection';
import { GameGlobalSection } from './settings/GameGlobalSection';
import { PluginsSection } from './settings/PluginsSection';

const SECTIONS: Array<{ id: SettingsSection; label: string; hint: string }> = [
  { id: 'app', label: 'APP 设置', hint: '个性化设置' },
  { id: 'game', label: '游戏全局设置', hint: '所有版本的默认值' },
  { id: 'plugins', label: '插件', hint: '内置能力的启停' },
];

function SectionBody({ section }: { section: SettingsSection }) {
  switch (section) {
    case 'game':
      return <GameGlobalSection />;
    case 'plugins':
      return <PluginsSection />;
    default:
      return <AppSection />;
  }
}

/**
 * 设置 Dialog。
 *
 * 之前「设置」是主区里的一个页签，和模组/存档并列——那会让"这个版本的游戏设置"
 * 与"这个程序自己的设置"挤在同一个位置。现在主区只谈游戏，设置单独开一层：
 * APP 与游戏全局在这里，版本级覆盖仍然在主区「游戏设置」页（优先级最高那一层）。
 */
export function SettingsDialog() {
  const open = useUI((s) => s.settingsOpen);
  const section = useUI((s) => s.settingsSection);
  const setSection = useUI((s) => s.setSettingsSection);
  const close = useUI((s) => s.closeSettings);

  return (
    <Dialog
      open={open}
      onClose={close}
      title="设置"
      subtitle="APP 与游戏全局设置在这里改动；版本单独设置可以通过在「游戏设置」页内覆盖全局设置。"
      width={880}
      bodyClassName="min-h-0 overflow-hidden"
    >
      {/* vh 不受整页缩放影响，除以缩放比例后这里才是「窗口高度的 72%」 */}
      <div
        className="flex min-h-[400px] items-stretch"
        style={{ height: 'calc(min(72vh, 620px) / var(--ui-zoom, 1))' }}
      >
        <nav
          aria-label="设置分区"
          className="h-full w-[176px] shrink-0 overflow-y-auto border-r border-line p-2"
        >
          {SECTIONS.map((item) => {
            const active = item.id === section;
            return (
              <button
                key={item.id}
                type="button"
                aria-current={active ? 'page' : undefined}
                onClick={() => setSection(item.id)}
                className={cx(
                  'mb-0.5 flex w-full select-none flex-col items-start rounded-[6px] px-2.5 py-1.5 text-left transition-colors duration-150',
                  active ? 'bg-hover text-ink' : 'text-ink3 hover:bg-hover hover:text-ink2',
                )}
              >
                <span className="text-[12px] font-medium">{item.label}</span>
                <span className="mt-[1px] text-[10px] leading-[13px] text-ink4">{item.hint}</span>
              </button>
            );
          })}
        </nav>

        <div
          key={section}
          className="anim-fade-in min-h-0 min-w-0 flex-1 overflow-y-auto"
        >
          <SectionBody section={section} />
        </div>
      </div>
    </Dialog>
  );
}
