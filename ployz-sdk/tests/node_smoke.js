"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
const relayUrl = process.env.PLOYZ_RELAY_URL;
const bearer = process.env.PLOYZ_BEARER;
const machineId = process.env.PLOYZ_MACHINE_ID;
const unknownMachineId = process.env.PLOYZ_UNKNOWN_MACHINE_ID;

if (!addon || !pkg || !relayUrl || !bearer || !machineId || !unknownMachineId) {
  throw new Error("Node smoke is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-smoke-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

function parseRpc(error) {
  try {
    return JSON.parse(error.message);
  } catch {
    throw new Error(`error is not generated RpcError JSON: ${error && error.message}`);
  }
}

async function expectRpc(fn, code) {
  try {
    await fn();
    throw new Error(`expected ${code}`);
  } catch (error) {
    const rpc = parseRpc(error);
    if (rpc.code !== code) {
      throw new Error(`expected ${code}, got ${rpc.code}: ${rpc.message}`);
    }
  }
}

(async () => {
  const client = await sdk.connect({ relayUrl, bearer, machineId });
  const about = await client.about();
  if (!Array.isArray(about.capabilities)) {
    throw new Error("about() must return capabilities");
  }
  if (!about.capabilities.includes("ployz.rpc.describe-contract.v1")) {
    throw new Error("callers must be able to branch on capability names");
  }

  const intent = {
    target: [
      {
        name: "web",
        mode: { mode: "replicated", replicas: 1 },
        container: { image: "nginx", pull_policy: "always" },
      },
    ],
    apply: [{ name: "web" }],
    options: {
      force_recreate: false,
      skip_health_monitor: true,
      placement_seed: 0,
    },
  };
  const preview = await client.preview(intent);
  if (!Array.isArray(preview.operations) || !Array.isArray(preview.warnings)) {
    throw new Error("preview() must return operations and warnings");
  }
  if (preview.operations.length !== 1) {
    throw new Error(
      `expected one previewed operation, got ${preview.operations.length}`,
    );
  }
  const outcome = await client.deploy(intent);
  if (!outcome.Success || !Array.isArray(outcome.Success.completed)) {
    throw new Error(`expected Success outcome, got ${JSON.stringify(outcome)}`);
  }
  if (outcome.Success.completed.length !== 1) {
    throw new Error(
      `expected one completed operation, got ${outcome.Success.completed.length}`,
    );
  }
  await expectRpc(() => client.deploy({ not: "a DeployIntent" }), "invalid_argument");
  const after = await client.about();
  if (!after.capabilities.includes("ployz.rpc.describe-contract.v1")) {
    throw new Error("Client must stay usable after deploy");
  }

  await client.close();
  await expectRpc(() => client.about(), "unavailable");
  await expectRpc(() => client.deploy(intent), "unavailable");
  await client.close();

  const again = await sdk.connect({ relayUrl, bearer, machineId });
  await again.about();
  await again.close();

  await expectRpc(
    () => sdk.connect({ relayUrl, bearer: "wrong-secret", machineId }),
    "unauthenticated",
  );
  await expectRpc(
    () => sdk.connect({ relayUrl, bearer, machineId: unknownMachineId }),
    "not_found",
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
  if (typeof sdk.Client.prototype.deploy !== "function") {
    throw new Error("Client.deploy must be a method");
  }
  if (typeof sdk.Client.prototype.watch !== "undefined") {
    throw new Error("Client.watch must not exist");
  }
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
