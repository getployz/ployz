import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import * as api from '@ployz/sdk/config';

// The workspace package routes config through WASM and RPC through napi, nothing else.
const require = createRequire(import.meta.url);
assert.equal(require.resolve('@ployz/sdk/config'), fileURLToPath(new URL('../config.mjs', import.meta.url)));
assert.deepEqual(Object.keys(require.cache).filter(file => file.endsWith('.node')), []);
assert.equal(typeof require('@ployz/sdk').connect, 'function');

// The browser checks a Setting the way core does before the Store sees it.
assert.deepEqual(Object.keys(api), ['parseEnvFile', 'parseServiceSetting']);
assert.equal(api.parseServiceSetting('replicas', 3), 3);
assert.throws(() => api.parseServiceSetting('replicas', 0));
assert.throws(() => api.parseServiceSetting('replicas', 51));
assert.throws(() => api.parseServiceSetting('privateDns', 'Invalid_DNS'));

// The raw editor reads .env text the way `ployz set --from-env-file` does.
assert.deepEqual(api.parseEnvFile('export A=1\nB="x\ny"'), [{ key: 'A', value: '1' }, { key: 'B', value: 'x\ny' }]);
assert.throws(() => api.parseEnvFile('=bad'), /Line 1/);
assert.equal(api.parseServiceSetting('imageReference', 'nginx:1.27'), 'nginx:1.27');
assert.throws(() => api.parseServiceSetting('imageReference', 'not a valid image ref!!'));
for (const field of ['command', 'startCommand', 'preDeployCommand']) {
  assert.throws(() => api.parseServiceSetting(field, 'private\u0000value'));
}
assert.equal(api.parseServiceSetting('command', ' echo café\nprintf ok '), 'echo café\nprintf ok');
for (const path of ['/ready\u0001probe', '/ready\tprobe', '/ready\nprobe', '/ready\u0085probe']) {
  assert.throws(() => api.parseServiceSetting('healthcheckPath', path));
}
assert.equal(api.parseServiceSetting('healthcheckPath', '/café?escaped=%0A'), '/café?escaped=%0A');
