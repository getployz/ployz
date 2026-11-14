"use strict";
const { test } = require("node:test");
const assert = require("node:assert/strict");
// The browser entry: ployz-core as WASM, the code the dashboard signs with.
const authority = import("../authority.mjs");

const IMAGE = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001";
const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
// `contained_json`, the fixture a session approves in ployz-core's authority/deploy_tests.rs, and its digest there.
const GOLDEN = "b38204e1d5b8a6d94b145b97d8dcbff4b50f5218b1cd9969324a1108aed6541b";
const web = (image = IMAGE) => ({
  name: "web",
  mode: { mode: "replicated", replicas: 2 },
  container: {
    image,
    pull_policy: "missing",
    command: ["nginx", "-g", "daemon off;"],
    entrypoint: ["/docker-entrypoint.sh"],
    environment: { PLAIN: "one", SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
    sealed_identity: {
      organization: "org",
      environment: "11111111-1111-1111-1111-111111111111",
      service: "22222222-2222-2222-2222-222222222222",
    },
    config_mounts: [{ config_name: "site", target: "/etc/site.conf", mode: 420 }],
  },
  ports: [{ mode: "ingress", hostname: "web.example.com", load_balancer_port: 443, container_port: 80, http_protocol: "http" }],
  volumes: [{ reference: "data", source: { kind: "ordinary", name: "data", driver: { name: "local", options: { type: "tmpfs" } } } }],
  mounts: [{ volume: "data", target: "/data" }],
  configs: [{ name: "site", content: [104, 105] }],
  pre_deploy: { command: ["migrate"], privileged: false },
  update: { order: "start_first" },
});
const preview = (target) => ({
  environment: { id: "33333333-3333-3333-3333-333333333333", project: "shop", name: "production", revision: 1 },
  version: "v1",
  namespace: "app",
  changes: [],
  unresolved: [],
  ...(target === undefined ? {} : { target }),
});
const terms = {
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  operation: "0f8fad5bd9cb469fa16570867728950e",
  machines: ["2".repeat(32), "1".repeat(32), "2".repeat(32)],
  issued_at: 100000,
  expires_at: 1000000,
  signer: { browser: "07".repeat(32) },
};

test("the body names the fixture's golden digest, in canonical form", async () => {
  const { deployApprovalBody } = await authority;
  const { body, changes, lines } = deployApprovalBody(preview([web()]), terms);
  const parsed = JSON.parse(body);
  assert.deepEqual(parsed.services, [{ name: "web", digest: GOLDEN }]);
  assert.deepEqual(parsed.machines, ["1".repeat(32), "2".repeat(32)]);
  assert.equal(parsed.namespace, "app");
  assert.equal(parsed.proposal, undefined);
  assert.deepEqual(Object.keys(parsed), [...Object.keys(parsed)].sort());
  assert.equal(body, JSON.stringify(parsed));
  assert.equal(changes[0].digest, GOLDEN);
  assert.deepEqual(lines, [
    `Approved web: ${IMAGE} (hook migrate)`,
    '  variables PLAIN=one, SECRET (sealed)',
    '  pre-deploy ["migrate"]',
    '  configs [{"content":"hi","name":"site"}]',
    '  mounts [{"no_copy":false,"read_only":false,"target":"/data","volume":"data"}]',
    '  ports [{"container_port":80,"hostname":"web.example.com","http_protocol":"http","load_balancer_port":443,"mode":"ingress"}]',
    '  volumes [{"reference":"data","source":{"driver":{"name":"local","options":{"type":"tmpfs"}},"kind":"ordinary","labels":{},"name":"data"}}]',
    '  command ["nginx","-g","daemon off;"]',
    '  config_mounts [{"config_name":"site","mode":420,"target":"/etc/site.conf"}]',
    '  entrypoint ["/docker-entrypoint.sh"]',
    '  pull_policy "missing"',
    "  pre-deploy privileged false",
  ]);
});

test("a proposal step names its place in the body", async () => {
  const { deployApprovalBody } = await authority;
  const proposal = { operation: "e".repeat(32), index: 1 };
  const { body } = deployApprovalBody(preview([web()]), { ...terms, proposal });
  assert.deepEqual(JSON.parse(body).proposal, proposal);
});

test("a built image, a tag image, a plan read without target, host access, or unknown terms sign nothing", async () => {
  const { deployApprovalBody } = await authority;
  assert.throws(() => deployApprovalBody(preview([web(`ployz-build/web@${IMAGE.split("@")[1]}`)]), terms), {
    message: "web builds from source, and servers that enforce device approval can't run a build yet. Deploy it from an image pinned by digest.",
  });
  assert.throws(() => deployApprovalBody(preview([web("nginx:1.29-alpine")]), terms), {
    message: "The dashboard can't check what web's tag nginx:1.29-alpine points to. Write web's image by digest, which docker buildx imagetools inspect nginx:1.29-alpine prints.",
  });
  assert.throws(() => deployApprovalBody(preview(), terms), { message: "Read the plan with target set to sign a deploy." });
  const root = { ...web(), volumes: [{ reference: "root", source: { kind: "bind", machine_path: "/" } }], mounts: [{ volume: "root", target: "/host" }] };
  assert.throws(() => deployApprovalBody(preview([root]), terms), {
    message: "web (binds /) reaches the server itself, so only an approved device can approve it. Run ployz deploy on that device.",
  });
  assert.throws(() => deployApprovalBody(preview([web()]), { ...terms, extra: 1 }), /unknown field `extra`/);
});

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
