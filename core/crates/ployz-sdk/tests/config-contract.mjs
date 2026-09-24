import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import * as api from '@ployz/sdk/config';

// The workspace package routes config through WASM and RPC through napi, nothing else.
const require = createRequire(import.meta.url);
assert.equal(require.resolve('@ployz/sdk/config'), fileURLToPath(new URL('../config.mjs', import.meta.url)));
assert.deepEqual(Object.keys(require.cache).filter(file => file.endsWith('.node')), []);
assert.equal(typeof require('@ployz/sdk').connect, 'function');

const input = {
  version: 2, privateDns: 'api',
  source: {
    version: 2, type: 'git', repository: 'acme/api', repositoryId: 42,
    access: { type: "github-installation", installationId: 7 }, rootDir: ' /apps/api/ ',
    branch: { type: 'connected', name: 'main' },
  },
  preDeployCommand: null, startCommand: null,
  healthcheck: { type: 'none' }, restartPolicy: 'unless-stopped',
};
const baseline = api.parseServiceConfig(input);
assert.equal(baseline.source.rootDir, '/apps/api');
const current = structuredClone(baseline);
current.source.access.installationId = 9;
current.startCommand = 'npm start';
const changes = api.compareServiceSettings(current, baseline);
assert.deepEqual(changes.map(row => row.path), ['source.repository', 'startCommand']);
const restored = api.restoreServiceSetting(current, baseline, 'source.repository');
assert.equal(restored.source.access.installationId, 7);
assert.equal(restored.startCommand, 'npm start');
const domains = api.parseServiceConfig({ ...baseline, routes: [
  { id: '11111111-1111-4111-8111-111111111111', hostname: 'first.example.test', targetPort: 3000 },
  { id: '22222222-2222-4222-8222-222222222222', hostname: 'last.example.test', targetPort: 3000 },
] });
const editedDomains = structuredClone(domains);
editedDomains.routes[0].targetPort = 8080;
assert.deepEqual(api.restoreServiceSetting(editedDomains, domains, `routes.${domains.routes[0].id}`).routes, domains.routes);

assert.throws(() => api.parseServiceSetting('replicas', 51));
assert.throws(() => api.parseServiceSetting('privateDns', 'Invalid_DNS'));
assert.throws(() => api.restoreServiceSetting(current, baseline, 'unknown'));
const secretBaseline = api.parseServiceConfig({ ...baseline, env: {
  TOKEN: { kind: 'secret', fingerprint: 'private-before' },
}});
const secretCurrent = api.parseServiceConfig({ ...baseline, env: {
  TOKEN: { kind: 'secret', fingerprint: 'private-after' },
}});
const redacted = api.compareServiceSettings(secretCurrent, secretBaseline);
assert.equal(redacted[0].path, 'env.TOKEN');
assert.equal(JSON.stringify(redacted).includes('private-'), false);
const resolved = api.resolveVariables({
  selfOwnerId: 'service',
  parts: [{ kind: 'ref', owner: { scope: 'self' }, key: 'TOKEN' }],
  producers: [{ ownerId: 'service', owner: { scope: 'service', lineageId: 'lineage' }, key: 'TOKEN', value: { kind: 'secret', value: 'private-value' } }],
});
assert.equal(resolved.status, 'resolved');
assert.equal(resolved.secret, true);
assert.equal(resolved.value, 'private-value');
const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const { env, mounts, ...settings } = baseline;
const intent = api.parseEnvironmentIntent({
  version: 1, environmentSlug: 'production',
  services: [{ id: id(1), lineageId: id(2), slug: 'api-display-slug', config: settings,
    variables: [], volumeAttachments: [] }],
  volumes: [],
});
const compiled = api.compileEnvironmentIntent(id(3), intent);
assert.equal(compiled.nodeSnapshots[0].config.privateDns, 'api');
assert.equal('name' in compiled.nodeSnapshots[0].config, false);
assert.equal(compiled.variableProducers.find(v => v.key === 'PLOYZ_PRIVATE_DOMAIN').value.value, 'api.internal');
const changed = structuredClone(intent);
changed.services[0].config.startCommand = 'new command';
const reverted = api.restoreEnvironmentNode(changed, intent, { nodeType: 'service', nodeId: id(1) }, 'startCommand');
assert.equal(reverted.services[0].config.startCommand, null);
assert.equal('env' in reverted.services[0].config, false);
const volumeRows = api.compareResourceSettings('volume', { version: 2, name: 'Renamed' }, { version: 2, name: 'Data' });
assert.equal(volumeRows[0].kind, 'update');
const rendered = api.renderVariableParts([{ kind: 'ref', owner: { scope: 'service', lineageId: id(2) }, key: 'HOST' }], { [id(2)]: 'renamed' });
assert.equal(rendered, '${{ renamed.HOST }}');
const nodes = value => value.nodeSnapshots.map(snapshot => ({
  node: { type: snapshot.nodeType, id: snapshot.nodeId }, config: snapshot.config,
}));
const review = api.projectEnvironmentChanges({
  working: { token: 'working', nodes: nodes(api.compileEnvironmentIntent(id(3), changed)) },
  applied: { token: 'applied', nodes: nodes(compiled) },
  saved: null,
  nodeIntroductions: { token: 'introductions', nodes: [] }, submitted: null,
});
const setting = review.groups[0].settings[0];
assert.equal(setting.kind, 'add');
assert.equal(setting.canRestore, true);
assert.equal(review.totalCount, 1);
const savedCreation = api.projectEnvironmentChanges({
  working: { token: 'working', nodes: nodes(api.compileEnvironmentIntent(id(3), changed)) },
  applied: { token: 'applied:none', nodes: [] },
  saved: { token: 'saved', nodes: nodes(compiled) },
  nodeIntroductions: { token: 'introductions', nodes: nodes(compiled) }, submitted: null,
});
assert.equal(savedCreation.groups[0].comparison, null);
assert.deepEqual(savedCreation.groups[0].settings, []);
assert.equal(savedCreation.totalCount, 1);
console.log('SDK config runs on WASM; napi carries RPC only.');
