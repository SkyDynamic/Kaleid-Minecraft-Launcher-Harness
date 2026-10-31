import type { PluginInfo } from '../../bridge/types';
import { Dot, type DotTone } from '../../components/Dot';
import { Switch } from '../../primitives';
import { useApp } from '../../store/app';
import { isRequiredPlugin, requiredReason } from './requiredPlugins';

function pluginTone(plugin: PluginInfo): DotTone {
  switch (plugin.state) {
    case 'ready':
      return 'green';
    case 'starting':
      return 'amber';
    case 'failed':
      return 'red';
    case 'registered':
      return 'accent';
    default:
      return 'gray';
  }
}

function stateLabel(plugin: PluginInfo): string {
  switch (plugin.state) {
    case 'ready':
      return '运行中';
    case 'starting':
      return '启动中';
    case 'failed':
      return '失败';
    case 'registered':
      return '已注册';
    default:
      return '已停用';
  }
}

/**
 * 插件管理（从右下角搬进设置 Dialog）。
 *
 * 必需插件不是"不给关"这么简单：`Switch` 直接 disabled，同时在旁边给出
 * **为什么不能关**——把开关置灰却不解释，用户只会以为界面坏了。
 */
export function PluginsSection() {
  const plugins = useApp((s) => s.plugins);
  const togglePlugin = useApp((s) => s.togglePlugin);
  const enabledCount = plugins.filter((p) => p.enabled).length;

  return (
    <div>
      <div className="flex items-baseline justify-between gap-3 border-b border-line px-4 py-2.5">
        <span className="text-[11px] text-ink4">
          停用插件会立刻回收它提供的界面与能力；必需插件不可停用。
        </span>
        <span className="shrink-0 font-mono text-[10px] leading-none text-ink4">
          {enabledCount} / {plugins.length}
        </span>
      </div>

      <div className="px-2 py-1.5">
        {plugins.length === 0 ? (
          <p className="px-2 py-4 text-[11.5px] text-ink4">还没有装载任何插件</p>
        ) : null}

        {plugins.map((plugin) => {
          // 后端 `required` 才是权威判断；静态表只负责补 tooltip 的理由文案。
          const required = plugin.required || isRequiredPlugin(plugin.id);
          const reason = requiredReason(plugin.id);

          return (
            <div
              key={plugin.id}
              className="flex items-center gap-2.5 rounded-[6px] px-2 py-2 transition-colors duration-150 hover:bg-hover"
            >
              <Dot tone={pluginTone(plugin)} size={6} />

              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5">
                  <span
                    className={
                      plugin.enabled
                        ? 'truncate text-[12px] text-ink2'
                        : 'truncate text-[12px] text-ink4'
                    }
                  >
                    {plugin.name}
                  </span>

                  {required ? (
                    <span className="group/chip relative shrink-0">
                      <span
                        tabIndex={0}
                        title={reason}
                        className="cursor-help select-none rounded-[4px] border border-line bg-layer2 px-1 py-[1px] text-[9.5px] leading-[13px] text-ink4 transition-colors group-hover/chip:text-ink3"
                      >
                        必需
                      </span>
                      <span
                        role="tooltip"
                        className="pointer-events-none absolute bottom-full left-1/2 z-20 mb-1.5 hidden w-[228px] -translate-x-1/2 rounded-[6px] border border-line-strong bg-layer2 px-2 py-1.5 text-[10.5px] leading-[15px] text-ink2 shadow-[0_8px_20px_rgba(0,0,0,.55)] group-hover/chip:block group-focus-within/chip:block"
                      >
                        {reason}
                      </span>
                    </span>
                  ) : null}

                  <span className="ml-auto shrink-0 font-mono text-[9.5px] leading-none text-ink4">
                    {stateLabel(plugin)}
                  </span>
                </div>
                <p className="mb-0 mt-0.5 truncate text-[10.5px] leading-[14px] text-ink4">
                  {plugin.description}
                </p>
              </div>

              <Switch
                ariaLabel={`${plugin.enabled ? '停用' : '启用'} ${plugin.name}`}
                checked={plugin.enabled}
                disabled={required}
                onChange={(checked) => void togglePlugin(plugin.id, checked)}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}
