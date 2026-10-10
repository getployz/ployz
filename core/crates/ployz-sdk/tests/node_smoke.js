"use strict";

const fs = require("node:fs");
const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");
const expectRpcError = require("./expect-rpc-error");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
const socketDirectory = process.env.PLOYZ_SOCKET_DIRECTORY;
const connectionsFor = (id) => [{ unix: path.join(socketDirectory, `${id}.sock`) }];
const machineId = process.env.PLOYZ_MACHINE_ID;
const unknownMachineId = process.env.PLOYZ_UNKNOWN_MACHINE_ID;
const configText = "C6_PRIVATE_CONFIG_SENTINEL\n";
const configBytes = Array.from(Buffer.from(configText));
const configName = "sentry/app.conf";
const configTarget = "/etc/sentry/app.conf";

function assertPrivatePreview(preview) {
  const serialized = JSON.stringify(preview);
  assert.ok(!serialized.includes(configText.trim()), "public preview omits Config text");
  assert.ok(!serialized.includes(JSON.stringify(configBytes)), "public preview omits Config bytes");
  const spec = preview.operations[0].operation.spec;
  assert.equal(spec.name, "web");
  assert.deepEqual(spec.configs, [{ name: configName, content: [] }]);
  assert.equal(spec.container.config_mounts[0].config_name, configName);
  assert.equal(spec.container.config_mounts[0].target, configTarget);
}

if (!addon || !pkg || !socketDirectory || !machineId || !unknownMachineId) {
  throw new Error("Node smoke is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-smoke-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(path.join(pkg, "runtime-logs.js"), path.join(dir, "runtime-logs.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

async function expectRpc(fn, code) {
  try {
    await fn();
    throw new Error(`expected ${code}`);
  } catch (error) {
    const rpc = expectRpcError(sdk, error);
    if (rpc.code !== code) {
      throw new Error(`expected ${code}, got ${rpc.code}: ${rpc.message}`);
    }
  }
}

(async () => {
  const client = await sdk.connect({ connections: connectionsFor(machineId) });
  for (const invalid of ["", "private\ncommand", "private\rcommand", "private capability", "private\0capability", "x".repeat(16 * 1024)]) {
    await assert.rejects(sdk.connect({ connections: [{ management: invalid }] }),
      error => error instanceof sdk.RpcError && error.code === "invalid_argument" && !error.message.includes("private"));
  }
  const about = await client.about();
  if (!Array.isArray(about.capabilities)) {
    throw new Error("about() must return capabilities");
  }
  if (!about.capabilities.includes("ployz.rpc.describe-contract.v1")) {
    throw new Error("callers must be able to branch on capability names");
  }

  const intent = {
    namespace: "app",
    target: [
      {
        name: "web",
        mode: { mode: "replicated", replicas: 1 },
        container: { image: "nginx", pull_policy: "always", config_mounts: [{ config_name: configName, target: configTarget }] },
        configs: [{ name: configName, content: configBytes }],
      },
    ],
    options: {
      force_recreate: false,
      skip_health_monitor: true,
      placement_seed: 0,
      selected: [{ name: "web" }],
    },
  };
    const preview = await client.preview(intent);
    assertPrivatePreview(preview);
    if (!Array.isArray(preview.operations) || !Array.isArray(preview.warnings)) {
      throw new Error("preview() must return operations and warnings");
    }
    if (preview.operations.length !== 1) {
      throw new Error(
        `expected one previewed operation, got ${preview.operations.length}`,
      );
    }
    if (typeof preview.confirm !== "function") {
      throw new Error("preview.confirm must be a method");
    }
    const running = preview.confirm();
    let sawProgress = false;
    for await (const event of running) {
      if (event && event.type === "progress") {
        sawProgress = true;
      }
    }
    if (!sawProgress) {
      throw new Error("confirm() must stream progress events");
    }
    const outcome = await running.finished;
    if (outcome.type !== "success" || !Array.isArray(outcome.completed)) {
      throw new Error(`expected success outcome, got ${JSON.stringify(outcome)}`);
    }
    if (outcome.completed.length !== 1) {
      throw new Error(
        `expected one completed operation, got ${outcome.completed.length}`,
      );
    }
    await expectRpc(() => preview.confirm(), "invalid_argument");
    await expectRpc(() => client.run({ not: "a DeployIntent" }), "invalid_argument");
    await expectRpc(() => client.clearManagementClient("Cloud"), "invalid_argument");
    await expectRpc(() => client.setManagementClient("Cloud"), "invalid_argument");
    await expectRpc(() => client.requestMachineUpgrade("worker", "not a request"), "invalid_argument");
    await expectRpc(() => client.inspectMachineUpgrade("worker", "not a request"), "invalid_argument");
  const preparation = client.prepare({
    deployment: {
      namespace: "app",
      snapshots: [{ config: {
        version: 2, privateDns: "web",
        source: { version: 1, type: "image", image: "nginx", credentials: { type: "none" } },
        preDeployCommand: null, startCommand: null,
        healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
        configs: [{ configResourceId: "sentry-id", configName: "sentry", mountDir: "/etc/sentry" }],
      } }],
      configs: [{ configResourceId: "sentry-id", name: "sentry", files: {
        "app.conf": { content: configText, mode: "0444", uid: 0, gid: 0 },
      } }],
    },
    sources: {},
  });
  const prepared = await preparation.finished;
  assertPrivatePreview(prepared);
  const preparedOutcome = await prepared.confirm().finished;
  assert.equal(preparedOutcome.type, "success", JSON.stringify(preparedOutcome));
  const after = await client.about();
  if (!after.capabilities.includes("ployz.rpc.describe-contract.v1")) {
    throw new Error("Client must stay usable after deploy");
  }

  await client.close();
  await expectRpc(() => client.about(), "unavailable");
  await expectRpc(() => client.run(intent), "unavailable");
  await client.close();

  const again = await sdk.connect({ connections: connectionsFor(machineId) });
  await again.about();
  await again.close();

  await expectRpc(
    () => sdk.connect({ connections: connectionsFor(unknownMachineId) }),
    "internal",
  );

  const forbidden = [
    "call",
    "request",
    "watch",
    "preview",
    "deploy",
    "connectTcp",
    "connectSsh",
    "connectUnix",
  ];
  for (const name of forbidden) {
    if (Object.hasOwn(sdk, name)) {
      throw new Error(`${name} must not be exported`);
    }
  }
  if (typeof sdk.Client.prototype.preview !== "function") {
    throw new Error("Client.preview must be a method");
  }
  if (typeof sdk.Client.prototype.run !== "function") {
    throw new Error("Client.run must be a method");
  }
  if (typeof sdk.Client.prototype.deploy !== "undefined") {
    throw new Error("Client.deploy must not exist");
  }
  if (typeof sdk.applyAll !== "function" || typeof sdk.applyOne !== "function") {
    throw new Error("applyAll / applyOne must be exported");
  }
  if (typeof sdk.Client.prototype.watch !== "undefined") {
    throw new Error("Client.watch must not exist");
  }
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
