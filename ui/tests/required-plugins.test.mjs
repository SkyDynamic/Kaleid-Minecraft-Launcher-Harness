import assert from 'node:assert/strict';
import test from 'node:test';
import { mockBridge } from '../src/bridge/mock.ts';

// A missing backend-equivalent guard would allow a bridge caller to disable a required plugin.
test('mock bridge rejects disabling required plugins without changing state or emitting changes', async () => {
  const plugins = await mockBridge.invoke('plugins_list');
  const logger = plugins.find((plugin) => plugin.id === 'launcher-logger');
  assert.ok(logger?.required && logger.enabled, 'launcher logger is always enabled');
  const required = plugins.filter((plugin) => plugin.required);
  let changes = 0;
  const unsubscribe = await mockBridge.listen('plugins-changed', () => { changes += 1; });
  try {
    for (const plugin of required) {
      await assert.rejects(
        mockBridge.invoke('plugins_set_enabled', { id: plugin.id, enabled: false }),
        /必需插件/,
      );
    }
    assert.deepEqual(await mockBridge.invoke('plugins_list'), plugins);
    assert.equal(changes, 0);
  } finally {
    unsubscribe();
  }
});

// Required protection must not accidentally disable optional-plugin management.
test('mock bridge still allows disabling and re-enabling optional news plugin', async () => {
  const disabled = await mockBridge.invoke('plugins_set_enabled', { id: 'news', enabled: false });
  assert.equal(disabled.find((plugin) => plugin.id === 'news')?.enabled, false);
  const enabled = await mockBridge.invoke('plugins_set_enabled', { id: 'news', enabled: true });
  assert.equal(enabled.find((plugin) => plugin.id === 'news')?.enabled, true);
});
