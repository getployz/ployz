"use strict";
const { test } = require("node:test");
const assert = require("node:assert/strict");
const sdk = require("..");

const IMAGE = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001";
const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
// `PayloadDigest`'s golden fixture in ployz-core's authority/deploy_tests.rs, and its digest there.
const GOLDEN = "cb8d3c6ef656772fbe9764188fc4ff7b7059e682de60a7686263ab386cd4ffa5";
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
    cap_add: ["NET_ADMIN"],
    resources: { devices: [{ machine_path: "/dev/null", container_path: "/dev/null", cgroup_permissions: "rwm" }] },
    sysctls: { "net.core.somaxconn": "1024" },
    config_mounts: [{ config_name: "site", target: "/etc/site.conf", mode: 420 }],
  },
  ports: [
    { mode: "host", bind: { kind: "all" }, published_port: 8080, container_port: 80, transport_protocol: "tcp" },
    { mode: "ingress", hostname: "web.example.com", load_balancer_port: 443, container_port: 80, http_protocol: "http" },
  ],
  volumes: [
    { reference: "host", source: { kind: "bind", machine_path: "/srv/web" } },
    { reference: "data", source: { kind: "ordinary", name: "data", driver: { name: "local", options: { type: "tmpfs" } } } },
  ],
  mounts: [{ volume: "host", target: "/srv" }, { volume: "data", target: "/data" }],
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

test("the body names the fixture's golden digest, in canonical form", () => {
  const { body, changes, lines } = sdk.deployApprovalBody(preview([web()]), terms);
  const parsed = JSON.parse(body);
  assert.deepEqual(parsed.services, [{ name: "web", digest: GOLDEN }]);
  assert.deepEqual(parsed.machines, ["1".repeat(32), "2".repeat(32)]);
  assert.equal(parsed.namespace, "app");
  assert.equal(parsed.proposal, undefined);
  assert.deepEqual(Object.keys(parsed), [...Object.keys(parsed)].sort());
  assert.equal(body, JSON.stringify(parsed));
  assert.equal(changes[0].digest, GOLDEN);
  assert.deepEqual(lines, [
    "Approved web: nginx@sha256:000000000000… (device /dev/null:/dev/null, adds NET_ADMIN, " +
      "sysctl net.core.somaxconn, host port 8080, binds /srv/web, hook migrate)",
  ]);
});

test("a proposal step names its place in the body", () => {
  const proposal = { operation: "e".repeat(32), index: 1 };
  const { body } = sdk.deployApprovalBody(preview([web()]), { ...terms, proposal });
  assert.deepEqual(JSON.parse(body).proposal, proposal);
});

test("a built image, a tag image, a plan read without target, or unknown terms sign nothing", () => {
  assert.throws(() => sdk.deployApprovalBody(preview([web(`ployz-build/web@${IMAGE.split("@")[1]}`)]), terms), {
    code: "invalid_argument",
    message: "web builds from source, and servers that enforce device approval can't run a build yet. Deploy it from an image pinned by digest.",
  });
  assert.throws(() => sdk.deployApprovalBody(preview([web("nginx:1.29-alpine")]), terms), {
    name: "RpcError",
    code: "invalid_argument",
    message: "The dashboard can't check what web's tag nginx:1.29-alpine points to. Write the image by digest, or run ployz deploy, which pins it for you.",
  });
  assert.throws(() => sdk.deployApprovalBody(preview(), terms), { code: "invalid_argument" });
  assert.throws(() => sdk.deployApprovalBody(preview([web()]), { ...terms, extra: 1 }), { code: "invalid_argument" });
});
