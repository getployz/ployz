"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const expectRpcError = require("./expect-rpc-error");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
const relayUrl = process.env.PLOYZ_RELAY_URL;
const bearer = process.env.PLOYZ_BEARER;
const pairing = process.env.PLOYZ_PAIRING;
const machineId = process.env.PLOYZ_MACHINE_ID;
const machineA = process.env.PLOYZ_MACHINE_A;
const machineB = process.env.PLOYZ_MACHINE_B;

if (
  !addon ||
  !pkg ||
  !relayUrl ||
  !bearer ||
  !pairing ||
  !machineId ||
  !machineA ||
  !machineB
) {
  throw new Error("Node volume smoke is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-volumes-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
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

function volume(machine, name) {
  return { machine_id: machine, name };
}

(async () => {
  if (typeof sdk.Client.prototype.removeVolumes !== "function") {
    throw new Error("Client.removeVolumes must be a method");
  }

  const client = await sdk.connect({ relayUrl, bearer, pairing, machineId });

  const destroyed = await client.removeVolumes({
    volumes: [volume(machineA, "data")],
    force: false,
  });
  const assert = require("node:assert/strict");
  assert.deepEqual(destroyed, [{ id: volume(machineA, "data"), outcome: { status: "removed" } }]);

  const partial = await client.removeVolumes({
    volumes: [volume(machineA, "data"), volume(machineB, "data"), volume(machineB, "logs")],
  });
  assert.equal(partial.length, 3);
  assert.deepEqual(partial[0], destroyed[0]);
  for (const name of ["data", "logs"]) {
    const failure = partial.find((entry) => entry.id.machine_id === machineB && entry.id.name === name);
    assert.equal(failure.outcome.status, "failed");
    assert.equal(failure.outcome.error.code, "unavailable");
  }

  await expectRpc(
    () => client.removeVolumes({ volumes: ["data"] }),
    "invalid_argument",
  );
  await expectRpc(
    () => client.removeVolumes({ volumes: [{ name: "data" }] }),
    "invalid_argument",
  );

  await client.close();
  await expectRpc(
    () => client.removeVolumes({ volumes: [volume(machineA, "data")] }),
    "unavailable",
  );
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
