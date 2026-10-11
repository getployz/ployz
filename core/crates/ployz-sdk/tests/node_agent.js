"use strict";

const fs = require("node:fs");
const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");
const expectRpcError = require("./expect-rpc-error");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
const socketDirectory = process.env.PLOYZ_SOCKET_DIRECTORY;
const machineId = process.env.PLOYZ_MACHINE_ID;
if (!addon || !pkg || !socketDirectory || !machineId) {
  throw new Error("Node agent test is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-agent-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(path.join(pkg, "runtime-logs.js"), path.join(dir, "runtime-logs.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

async function expectCode(code, promise) {
  try {
    await promise;
  } catch (error) {
    assert.equal(expectRpcError(sdk, error).code, code);
    return;
  }
  throw new Error(`expected ${code}`);
}

(async () => {
  const client = await sdk.connect({ connections: [{ unix: path.join(socketDirectory, `${machineId}.sock`) }] });

  const { servers, seen } = await client.agentAsk({ query: "servers", only: null });
  assert.deepEqual(seen.unanswered, []);
  assert.deepEqual(
    servers.map(({ name, state, draining, namespaces }) => ({ name, state, draining, namespaces })),
    [{ name: "one", state: "online", draining: false, namespaces: ["app"] }],
  );

  const { servers: none } = await client.agentAsk({ query: "servers", only: "nowhere" });
  assert.deepEqual(none, []);

  const { containers } = await client.agentAsk({ query: "containers", namespace: "app", service: null });
  assert.deepEqual(
    containers.map(({ server, service, state, health, image }) => ({ server, service, state, health, image })),
    [{ server: "one", service: "web", state: "running", health: "healthy", image: "alpine:3.23.3" }],
  );
  const { containers: other } = await client.agentAsk({ query: "containers", namespace: "app", service: "api" });
  assert.deepEqual(other, []);

  await expectCode("unsupported", client.agentAsk({ query: "github_connection" }));
  await expectCode("invalid_argument", client.agentAsk({ query: "nowhere" }));
  await client.close();

  assert.deepEqual(sdk.agentCloudCall({ ask: { query: "servers" } }), { ask: { query: "servers", only: null } });
  for (const malformed of [{ ask: { query: "bogus" } }, { act: { action: { action: "drain" } } }, { ask: {}, act: {} }]) {
    await expectCode("invalid_argument", (async () => sdk.agentCloudCall(malformed))());
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
