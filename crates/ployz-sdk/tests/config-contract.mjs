import assert from 'node:assert/strict';
import * as native from '../config.js';
import * as browser from '../config-browser.mjs';

const input = {
  version: 2, name: 'API', privateDns: 'api',
  source: {
    version: 2, type: 'git', repository: 'acme/api', repositoryId: 42,
    installationId: 7, rootDir: ' /apps/api/ ',
    branch: { type: 'connected', name: 'main' }, autoDeploy: true, waitForCi: false,
  },
  preDeployCommand: null, startCommand: null,
  healthcheck: { type: 'none' }, restartPolicy: 'unless-stopped',
};
const results = [];
for (const api of [native, browser]) {
  const baseline = api.parseServiceConfig(input);
  assert.equal(baseline.source.rootDir, '/apps/api');
  const current = structuredClone(baseline);
  current.source.installationId = 9;
  current.startCommand = 'npm start';
  const changes = api.compareServiceSettings(current, baseline);
  assert.deepEqual(changes.map(row => row.path), ['source.repository', 'startCommand']);
  const restored = api.restoreServiceSetting(current, baseline, 'source.repository');
  assert.equal(restored.source.installationId, 7);
  assert.equal(restored.startCommand, 'npm start');
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
  const { env, mounts, variableGroupAttachments, ...settings } = baseline;
  const intent = api.parseEnvironmentIntent({
    version: 1, environmentSlug: 'production',
    services: [{ id: id(1), lineageId: id(2), slug: 'api', config: settings,
      variables: [], variableGroupAttachments: [], volumeAttachments: [],
      encryptedRegistryUsername: null, encryptedRegistrySecret: null }],
    variableGroups: [], volumes: [],
  });
  const compiled = api.compileEnvironmentIntent(id(3), intent);
  assert.equal(compiled.nodeSnapshots[0].config.name, 'API');
  assert.equal(compiled.variableProducers.find(v => v.key === 'PLOYZ_PRIVATE_DOMAIN').value.value, 'api-production.internal');
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
    saved: { token: 'saved', nodes: nodes(compiled) },
    applied: { token: 'applied', nodes: nodes(compiled) },
    nodeIntroductions: { token: 'introductions', nodes: [] }, submitted: null,
  });
  const setting = review.groups[0].settings[0];
  assert.equal(setting.kind, 'add');
  assert.equal(setting.canRestore, true);
  assert.equal(review.totalCount, 1);
  results.push({ baseline, changes, restored, redacted, resolved, intent, compiled, reverted, volumeRows, rendered, review });
}
assert.deepEqual(results[0], results[1]);
console.log('Native and browser service configuration contracts agree.');
