"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

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
  if (destroyed.successes.length !== 1) {
    throw new Error(`expected one destroyed volume, got ${JSON.stringify(destroyed)}`);
  }
  if (destroyed.successes[0].machine_id !== machineA) {
    throw new Error(`destroyed volume Machine mismatch: ${JSON.stringify(destroyed)}`);
  }
  if (destroyed.successes[0].value !== "data") {
    throw new Error(`destroyed volume name mismatch: ${JSON.stringify(destroyed)}`);
  }
  if (destroyed.failures.length !== 0 || destroyed.omissions.length !== 0) {
    throw new Error(`successful removal must not fail or omit: ${JSON.stringify(destroyed)}`);
  }

  const partial = await client.removeVolumes({
    volumes: [volume(machineA, "data"), volume(machineB, "data")],
  });
  if (partial.successes.length !== 1 || partial.successes[0].machine_id !== machineA) {
    throw new Error(`expected Machine A success, got ${JSON.stringify(partial)}`);
  }
  if (partial.failures.length !== 1 || partial.failures[0].machine_id !== machineB) {
    throw new Error(`expected Machine B failure, got ${JSON.stringify(partial)}`);
  }
  if (partial.failures[0].error.code !== "unavailable") {
    throw new Error(`expected unavailable on Machine B, got ${JSON.stringify(partial)}`);
  }
  if (partial.omissions.length !== 0) {
    throw new Error(`partial failure must not omit: ${JSON.stringify(partial)}`);
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
