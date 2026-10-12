"use strict";

// Every agent tool, driven through the addon the way Cloud's sidebar drives it: Store
// asks and writes go to a real Store, Cloud asks and acts get canned answers. The
// Store and the answers carry numbers past u32, which napi hands over as floats.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
if (!addon || !pkg) {
  throw new Error("Agent sweep is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-sweep-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(path.join(pkg, "runtime-logs.js"), path.join(dir, "runtime-logs.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

const ORG = "acme";
const U32_MAX = 4294967295;
const NOW = Math.floor(Date.now() / 1000);
const TB = 3840000000000;
const db1 = { machine: "11111111111111111111111111111111", name: "db-1" };
const db2 = { machine: "22222222222222222222222222222222", name: "db-2" };
const web1 = { machine: "33333333333333333333333333333333", name: "web-1" };
const fresh = { seen_by: "web-1", observed_at: NOW, read_at: NOW, age_seconds: 0, from: "server", unanswered: [] };

const cloudAnswers = {
  github_connection: () => ({ connected: true, repositories: ["acme/api"], connect_at: "https://github.com/apps/ployz/installations/new" }),
  github_branches: () => ({ default_branch: "main", branches: ["main", "feature"] }),
  github_tree: (q) => ({ repository: q.repository, ref: "main", paths: ["src/main.rs", "src/lib.rs"], truncated: false }),
  github_file: (q) => ({ repository: q.repository, ref: "main", path: q.path, size: 5 * U32_MAX, content: "fn main() {}\n" }),
  servers: () => ({
    servers: [db1, db2, web1].map((server) => ({
      ...server,
      state: "online",
      release: "0.2.3",
      upgrade: null,
      draining: false,
      disk: { used_bytes: 2 * U32_MAX, total_bytes: TB },
      namespaces: server.name === "web-1" ? ["shop-old"] : [],
    })),
    seen: fresh,
  }),
  containers: (q) => ({
    containers: [{ server: "web-1", service: q.service ?? "web", state: "running", exit_code: null, health: "healthy", restarts: 0, image: "nginx:1.27", deployment: null }],
    servers: ["web-1"],
    seen: fresh,
  }),
  logs: () => ({ servers: [{ server: "web-1", text: { status: "withheld", why: "no_lines" } }], seen: fresh }),
  copies: () => ({ volumes: [{ volume: "pgdata", writer: db1, mirror: { server: db2, synced_at: NOW } }], seen: fresh }),
  operations: () => ({ operations: [operation("mirror")] }),
};

function operation(run) {
  return { attempt: "0198c6a0-0000-7000-8000-000000000001", run, state: "done", requested_at: NOW - 60, ended_at: NOW, reason: null, servers: [], kept: [], unanswered: [] };
}

function refusalOf(error) {
  if (!(error instanceof sdk.RpcError)) throw error;
  return { code: error.code, message: error.message, details: error.details ?? null };
}

const seen = { past: 0, unsafe: [] };
function walk(value, where) {
  if (typeof value === "number") {
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) seen.unsafe.push(where);
    if (value > U32_MAX) seen.past += 1;
  } else if (Array.isArray(value)) {
    value.forEach((item, i) => walk(item, `${where}[${i}]`));
  } else if (value !== null && typeof value === "object") {
    for (const [key, item] of Object.entries(value)) walk(item, `${where}.${key}`);
  }
}

async function answerAsk(store, ask) {
  if (ask.at === "cloud") {
    const answer = cloudAnswers[ask.ask.query];
    if (!answer) throw new Error(`no canned answer for Cloud ask ${ask.ask.query}`);
    return { value: answer(ask.ask) };
  }
  try {
    return { value: await store.read(ORG, ask.ask, TRUSTED) };
  } catch (error) {
    return { error: refusalOf(error) };
  }
}

async function answerWrite(store, write) {
  if (write.at === "act") return { value: operation(write.write.action.action) };
  try {
    return { value: await store.write(ORG, write.write, TRUSTED, "Sweep Member") };
  } catch (error) {
    return { error: refusalOf(error) };
  }
}

const TRUSTED = {
  repositories: [{
    repository: "acme/api",
    repository_id: 5 * U32_MAX,
    access: { type: "github-installation", installationId: 3 * U32_MAX },
    default_branch: "main",
    branches: ["main", "feature"],
  }],
  domains: { cluster_domain: { name: "acme.ployz.app", status: { kind: "ready" } }, certificates: null, ingress_addresses: [], lookups: [] },
  servers: 3,
  approval: "not_required",
};

async function call(store, tool, input, key) {
  let turn = sdk.agentStart(tool, input, { project: "shop", environment: null, store: { at: "cloud", organization: ORG } }, key);
  for (;;) {
    walk(turn, `${tool}.turn`);
    const { step, state } = turn;
    if (step.step === "done") return step.result;
    const answers =
      step.step === "write"
        ? [await answerWrite(store, step.write)]
        : await Promise.all((step.step === "read" ? step.asks : step.then).map((ask) => answerAsk(store, ask)));
    walk(answers, `${tool}.answers`);
    turn = sdk.agentResume(state, answers);
  }
}

const FIXTURES = [
  ["environment_show", {}],
  ["project_create", { name: "docs", request_key: "project-docs-0001" }],
  ["service_add", { name: "web", image: "nginx:1.27" }],
  ["service_add", { name: "db", image: "postgres:17" }],
  ["service_add", { name: "api", repository: "acme/api" }],
  ["volume_add", { name: "pgdata", size: "10GiB", service: "db", path: "/var/lib/postgresql/data" }],
  ["volume_update", { volume: "pgdata", shared_writes: true }],
  ["config_write", { config: "nginx", files: { "nginx.conf": "events {}" }, service: "web", path: "/etc/nginx" }],
  ["domain_add", { service: "web", hostname: "app.example.com" }],
  ["domain_add", { service: "web", prefix: "shop" }],
  ["domain_update", { domain: "shop", prefix: "store" }],
  ["service_inspect", { service: "web" }],
  ["service_update", { service: "web", set: { image: "nginx:1.28" } }],
  ["review", {}],
  ["publish", {}],
  ["deploy", { message: "first", request_key: "deploy-shop-0001" }],
  ["deployment_show", {}],
  ["deployment_show", { deployment: 2 * U32_MAX }, "not_found"],
  ["deployment_cancel", {}],
  ["environment_pr", { enabled: true, repository: "acme/api" }],
  ["environment_setup", {}],
  ["github_ls", { repository: "acme/api", path: "src" }],
  ["github_connect", {}],
  ["server_show", { server: "web-1" }],
  ["service_status", { service: "web" }],
  ["service_logs", { service: "web" }, "not_found"],
  ["volume_show", { volume: "pgdata" }],
  ["service_restart", { service: "web", request_key: "restart-web-0001" }, "not_found"],
  ["service_start", { service: "web", request_key: "start-web-0001" }, "not_found"],
  ["service_stop", { service: "web", request_key: "stop-web-0001" }, "not_found"],
  ["server_drain", { server: "web-1", wait_seconds: 0 }],
  ["server_upgrade", { server: "web-1", wait_seconds: 0 }],
  ["server_clean", { leftover: "shop-old" }],
  ["volume_mirror", { volume: "pgdata", server: "db-2", wait_seconds: 0 }],
  ["volume_move", { volume: "pgdata", to: "db-2", wait_seconds: 0 }],
  ["volume_mirror_remove", { volume: "pgdata" }],
  ["volume_release", { volume: "pgdata", wait_seconds: 0 }],
  ["config_remove", { config: "nginx", file: "nginx.conf" }],
  ["domain_remove", { domain: "store" }],
  ["service_rename", { service: "web", to: "www" }],
  ["volume_remove", { volume: "pgdata" }],
  ["service_remove", { service: "db" }],
  ["discard", {}],
  ["server_remove", { server: "web-1" }],
  ["environment_remove", { environment: "production", wait_seconds: 0 }, "unsupported"],
  ["project_remove", { project: "docs", wait_seconds: 0 }],
];

const INTERNAL = /unknown shape|came back short|did not encode/;

(async () => {
  const catalog = sdk.agentTools().tools.map((tool) => tool.name);
  const named = new Set(FIXTURES.map(([tool]) => tool));
  const missing = catalog.filter((tool) => !named.has(tool));
  const stray = [...named].filter((tool) => !catalog.includes(tool));
  if (missing.length > 0 || stray.length > 0) {
    throw new Error(`every catalog tool needs a fixture: missing ${missing.join(", ") || "none"}, unknown ${stray.join(", ") || "none"}`);
  }

  const store = await sdk.openConfigStore(`sqlite:${path.join(dir, "store.db")}`, "a sealing secret long enough for tests");
  await store.write(ORG, {
    command: "create_project",
    id: "00000000-0000-4000-8000-000000000001",
    name: "shop",
    default_environment: "00000000-0000-4000-8000-000000000002",
  });

  const failed = [];
  const report = [];
  for (const [i, [tool, input, expected = null]] of FIXTURES.entries()) {
    const result = await call(store, tool, input, `sweep-${i}`);
    const refusal = result.ok ? null : result.refusal;
    const got = refusal === null ? null : refusal.code;
    const internal = refusal !== null && (got === "internal" || INTERNAL.test(refusal.message));
    const pass = !internal && got === expected;
    if (!pass) failed.push(`${tool}: expected ${expected ?? "an answer"}, got ${refusal === null ? "an answer" : `${got}: ${refusal.message}`}`);
    report.push(`${pass ? "ok  " : "FAIL"} ${tool} ${refusal === null ? "answered" : `refused ${got}: ${refusal.message}`}`);
  }
  console.log(report.join("\n"));
  console.log(`${catalog.length} tools, ${FIXTURES.length} calls, ${failed.length} failed, ${seen.past} numbers past u32 crossed`);
  if (failed.length > 0) throw new Error(`tools did not answer as expected:\n${failed.join("\n")}`);
  if (seen.unsafe.length > 0) throw new Error(`integers past 2^53 crossed JavaScript: ${seen.unsafe.join(", ")}`);
  if (seen.past === 0) throw new Error("no number past u32 crossed JavaScript");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
