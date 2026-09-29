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
assert.deepEqual(Object.keys(api), ['parseServiceSetting']);
assert.equal(api.parseServiceSetting('replicas', 3), 3);
assert.throws(() => api.parseServiceSetting('replicas', 51));
assert.throws(() => api.parseServiceSetting('privateDns', 'Invalid_DNS'));
