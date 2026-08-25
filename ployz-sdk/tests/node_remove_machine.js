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
const workerMachine = process.env.PLOYZ_WORKER_MACHINE;
const emptyMachine = process.env.PLOYZ_EMPTY_MACHINE;

if (
  !addon ||
  !pkg ||
  !relayUrl ||
  !bearer ||
  !pairing ||
  !machineId ||
  !workerMachine ||
  !emptyMachine
) {
  throw new Error("Node Machine removal is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-remove-machine-"));
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

function dockerVolume(loss) {
  if (!loss || typeof loss !== "object" || !loss.DockerVolume) {
    throw new Error(`expected DockerVolume Data Loss, got ${JSON.stringify(loss)}`);
  }
  if ("kind" in loss || "scope" in loss || "name" in loss) {
    throw new Error(`Data Loss must not be a kind/name/scope bag: ${JSON.stringify(loss)}`);
  }
  return loss.DockerVolume;
}

(async () => {
  if (typeof sdk.Client.prototype.removeMachine !== "function") {
    throw new Error("Client.removeMachine must be a method");
  }
  if (typeof sdk.Client.prototype.confirmAll === "function") {
    throw new Error("no API may confirm a read's Data Loss without naming its entries");
  }

  const client = await sdk.connect({ relayUrl, bearer, pairing, machineId });

  const observed = await client.dataLossIfMachineRemoved(workerMachine);
  if (observed.data_loss.length !== 2) {
    throw new Error(`expected two Data Loss entries, got ${JSON.stringify(observed)}`);
  }

  try {
    await client.removeMachine(workerMachine, { confirmed: [] });
    throw new Error("unconfirmed removal must fail");
  } catch (error) {
    if (error.message === "unconfirmed removal must fail") {
      throw error;
    }
    const rpc = parseRpc(error);
    if (rpc.code !== "invalid_argument") {
      throw new Error(`expected invalid_argument, got ${JSON.stringify(rpc)}`);
    }
    if (!rpc.details || !Array.isArray(rpc.details.missing)) {
      throw new Error(`failure must name missing Data Loss, got ${JSON.stringify(rpc)}`);
    }
    const missing = rpc.details.missing.map((row) => dockerVolume(row).name).sort();
    if (missing.join(",") !== "data,logs") {
      throw new Error(`missing names mismatch: ${JSON.stringify(rpc)}`);
    }
    if (!rpc.message.includes("data") || !rpc.message.includes("logs")) {
      throw new Error(`message must name the missing Data Loss: ${rpc.message}`);
    }
  }

  try {
    await client.removeMachine(workerMachine, observed);
    throw new Error("ObservedDataLoss must not confirm a read");
  } catch (error) {
    if (error.message === "ObservedDataLoss must not confirm a read") {
      throw error;
    }
    const rpc = parseRpc(error);
    if (rpc.code !== "invalid_argument") {
      throw new Error(`expected invalid_argument for a read echo, got ${JSON.stringify(rpc)}`);
    }
  }

  const removed = await client.removeMachine(workerMachine, {
    confirmed: observed.data_loss,
  });
  if (removed.reset_warning != null) {
    throw new Error(`unexpected reset warning: ${JSON.stringify(removed)}`);
  }

  const empty = await client.removeMachine(emptyMachine, { confirmed: [] });
  if (empty.reset_warning != null) {
    throw new Error(`empty Machine removal must succeed: ${JSON.stringify(empty)}`);
  }

  await client.close();
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
