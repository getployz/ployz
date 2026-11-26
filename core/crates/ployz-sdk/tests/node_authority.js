"use strict";
const { test } = require("node:test");
const assert = require("node:assert/strict");
// The browser entry: ployz-core as WASM, the code the dashboard signs with.
const authority = import("../authority.mjs");

const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
const ruleSpec = (name, plain) => ({
  name,
  mode: { mode: "replicated", replicas: 1 },
  container: {
    image: `ployz-build/${name}`,
    pull_policy: "never",
    environment: { PLAIN: plain, SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
    sealed_identity: {
      organization: "org",
      environment: "11111111-1111-1111-1111-111111111111",
      service: "22222222-2222-2222-2222-222222222222",
    },
  },
  pre_deploy: { command: ["migrate"] },
});
const ruleSource = (service) => ({
  service,
  repository: "Ployz-test/lab-service",
  repository_id: 1450985696,
  branch: "main",
  root_dir: ".",
  build: { buildMethod: "dockerfile", dockerfilePath: "Dockerfile" },
});
const ruleView = {
  environment: { id: "33333333-3333-3333-3333-333333333333", project: "shop", name: "production", revision: 1 },
  namespace: "app",
  sources: [ruleSource("web")],
  target: [ruleSpec("web", "one"), ruleSpec("cache", "two")],
  changed: [],
};
// A Save's tap: no version, only the Source step drafted from `view`.
const saving = (view) => ({
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  namespace: view.namespace,
  services: [],
  operation: "44".repeat(16),
  issued_at: 1000,
  expires_at: 601000,
  seed: Array.from({ length: 16 }, () => 3),
  against: { trusted: {}, containers: [] },
  source: { rule: null, sources: view.sources, target: view.target, machines: ["2".repeat(32), "1".repeat(32)], owner_id: 278111222 },
});

test("a rule covers each Service that builds from GitHub, naming no particular passkey", async () => {
  const { versionSteps } = await authority;
  const { body, summary } = versionSteps(saving(ruleView)).source;
  assert.equal(JSON.parse(body).signer, "passkey");
  assert.deepEqual(summary.services.map((service) => service.name), ["web"]);
});

test("turning a rule off signs its next generation, and refuses with no rule on", async () => {
  const { autoDeployRuleEndBody, versionSteps } = await authority;
  const kept = { ...ruleView, rule: { body: versionSteps(saving(ruleView)).source.body, signatures: [] } };
  const off = JSON.parse(autoDeployRuleEndBody(kept, { issued_at: 2000, signer: "passkey" }).body);
  assert.deepEqual([off.generation, off.ended, off.issued_at], [2, true, 2000]);
  assert.throws(() => autoDeployRuleEndBody(ruleView, { issued_at: 2000, signer: "passkey" }), /no automatic deploy rule/);
});

test("a rule over 20 Services drafts in under 20 ms", async () => {
  const { versionSteps } = await authority;
  const names = Array.from({ length: 20 }, (_, index) => `web${index}`);
  const view = { ...ruleView, sources: names.map(ruleSource), target: names.map((name) => ruleSpec(name, name)) };
  versionSteps(saving(view));
  const times = Array.from({ length: 21 }, () => {
    const started = process.hrtime.bigint();
    versionSteps(saving(view));
    return Number(process.hrtime.bigint() - started) / 1e6;
  }).sort((a, b) => a - b);
  const median = times[10];
  console.log(`rule body for 20 Services: median ${median.toFixed(2)} ms, max ${times[20].toFixed(2)} ms`);
  assert.ok(median < 20, `median ${median} ms`);
});
