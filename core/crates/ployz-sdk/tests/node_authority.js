"use strict";
const { test } = require("node:test");
const assert = require("node:assert/strict");
// The browser entry: ployz-core as WASM, the code the dashboard signs with.
const authority = import("../authority.mjs");

const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
// `AutoDeployRule::draft`'s golden fixture in ployz-core's authority/rule_draft_tests.rs, and the digest of its body there.
const RULE_GOLDEN = "5fe1b138faa40462100905f967e762cae20eec664bf4a2c9e8e40066ec9fcd20";
const BUILDER_SHA = "39e1f488e492c0641e520777e481e8f5b266985b";
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
const ruleTerms = {
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  machines: ["2".repeat(32), "1".repeat(32), "2".repeat(32)],
  owner_id: 278111222,
  builder: { ref: `getployz/build/.github/workflows/build.yml@${BUILDER_SHA}`, sha: BUILDER_SHA },
  platform: "linux/amd64",
  new_rule: "33".repeat(16),
  issued_at: 1000,
  signer: "passkey",
};

test("a rule's body is the bytes the CLI drafts, naming no particular passkey", async () => {
  const { autoDeployRuleBody } = await authority;
  const { createHash } = require("node:crypto");
  const { body, summary } = autoDeployRuleBody(ruleView, ruleTerms);
  assert.equal(createHash("sha256").update(body).digest("hex"), RULE_GOLDEN);
  assert.equal(JSON.parse(body).signer, "passkey");
  assert.deepEqual(summary.services.map((service) => service.name), ["web"]);
  assert.throws(() => autoDeployRuleBody(ruleView, ruleTerms, ["cache"]), /cache doesn't build from GitHub/);
});

test("turning a rule off signs its next generation, and refuses with no rule on", async () => {
  const { autoDeployRuleBody, autoDeployRuleEndBody } = await authority;
  const on = autoDeployRuleBody(ruleView, ruleTerms);
  const kept = { ...ruleView, rule: { body: on.body, signatures: [] } };
  const off = JSON.parse(autoDeployRuleEndBody(kept, { issued_at: 2000, signer: "passkey" }).body);
  assert.deepEqual([off.generation, off.ended, off.issued_at], [2, true, 2000]);
  assert.throws(() => autoDeployRuleEndBody(ruleView, { issued_at: 2000, signer: "passkey" }), /no automatic deploy rule/);
});

test("a rule over 20 Services drafts in under 20 ms", async () => {
  const { autoDeployRuleBody } = await authority;
  const names = Array.from({ length: 20 }, (_, index) => `web${index}`);
  const view = { ...ruleView, sources: names.map(ruleSource), target: names.map((name) => ruleSpec(name, name)) };
  autoDeployRuleBody(view, ruleTerms);
  const times = Array.from({ length: 21 }, () => {
    const started = process.hrtime.bigint();
    autoDeployRuleBody(view, ruleTerms);
    return Number(process.hrtime.bigint() - started) / 1e6;
  }).sort((a, b) => a - b);
  const median = times[10];
  console.log(`rule body for 20 Services: median ${median.toFixed(2)} ms, max ${times[20].toFixed(2)} ms`);
  assert.ok(median < 20, `median ${median} ms`);
});
