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
const workerMachine = process.env.PLOYZ_WORKER_MACHINE;
const scratchMachineId = process.env.PLOYZ_SCRATCH_MACHINE_ID;
const shopMachineId = process.env.PLOYZ_SHOP_MACHINE_ID;

if (
  !addon ||
  !pkg ||
  !relayUrl ||
  !bearer ||
  !pairing ||
  !machineId ||
  !workerMachine ||
  !scratchMachineId ||
  !shopMachineId
) {
  throw new Error("Node Cluster destroy is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-destroy-cluster-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

function dockerVolume(loss) {
  if (!loss || typeof loss !== "object" || loss.kind !== "docker_volume" || !loss.id) {
    throw new Error(`expected docker_volume Data Loss, got ${JSON.stringify(loss)}`);
  }
  if ("scope" in loss || "name" in loss || "machine_id" in loss) {
    throw new Error(`Data Loss identity must nest under id, not spread beside kind: ${JSON.stringify(loss)}`);
  }
  return loss.id;
}

(async () => {
  if (typeof sdk.Client.prototype.destroyCluster !== "function") {
    throw new Error("Client.destroyCluster must be a method");
  }
  if (typeof sdk.Client.prototype.dataLossIfClusterDestroyed !== "function") {
    throw new Error("Client.dataLossIfClusterDestroyed must be a method");
  }
  if (typeof sdk.Client.prototype.confirmAll === "function") {
    throw new Error("no API may confirm a read's Data Loss without naming its entries");
  }

  const client = await sdk.connect({ relayUrl, bearer, pairing, machineId });

  const observed = await client.dataLossIfClusterDestroyed();
  if (!Array.isArray(observed.data_loss) || observed.data_loss.length !== 3) {
    throw new Error(`expected three Data Loss entries, got ${JSON.stringify(observed)}`);
  }
  const names = observed.data_loss.map((row) => dockerVolume(row).name).sort();
  if (names.join(",") !== "scratch,shop_data,shop_logs") {
    throw new Error(`unexpected Docker Volume names: ${names.join(",")}`);
  }
  const shop = observed.data_loss.filter((row) => dockerVolume(row).name.startsWith("shop"));
  for (const row of shop) {
    if (dockerVolume(row).machine_id !== shopMachineId) {
      throw new Error(`shop Data Loss must carry its Machine, got ${JSON.stringify(row)}`);
    }
  }
  const scratch = observed.data_loss.find((row) => dockerVolume(row).name === "scratch");
  if (!scratch || dockerVolume(scratch).machine_id !== scratchMachineId) {
    throw new Error(`scratch Data Loss must carry its Machine, got ${JSON.stringify(observed)}`);
  }

  try {
    await client.destroyCluster({ confirmed: [] });
    throw new Error("unconfirmed teardown must fail");
  } catch (error) {
    if (error.message === "unconfirmed teardown must fail") {
      throw error;
    }
    const rpc = expectRpcError(sdk, error);
    if (rpc.code !== "invalid_argument") {
      throw new Error(`expected invalid_argument, got ${JSON.stringify(rpc)}`);
    }
    if (!rpc.details || !Array.isArray(rpc.details.missing)) {
      throw new Error(`failure must name missing Data Loss, got ${JSON.stringify(rpc)}`);
    }
    const missing = rpc.details.missing.map((row) => dockerVolume(row).name).sort();
    if (missing.join(",") !== "scratch,shop_data,shop_logs") {
      throw new Error(`missing names mismatch: ${JSON.stringify(rpc)}`);
    }
  }

  try {
    await client.destroyCluster(observed);
    throw new Error("ObservedDataLoss must not confirm a read");
  } catch (error) {
    if (error.message === "ObservedDataLoss must not confirm a read") {
      throw error;
    }
    const rpc = expectRpcError(sdk, error);
    if (rpc.code !== "invalid_argument") {
      throw new Error(`expected invalid_argument for a read echo, got ${JSON.stringify(rpc)}`);
    }
  }

  const teardown = await client.destroyCluster({ confirmed: observed.data_loss });
  if (!teardown || teardown.pairing_revoked !== true) {
    throw new Error(`expected pairing revoked, got ${JSON.stringify(teardown)}`);
  }
  if (!Array.isArray(teardown.destroyed_projects) || !teardown.destroyed_projects.includes("shop")) {
    throw new Error(`expected shop destroyed, got ${JSON.stringify(teardown)}`);
  }
  if (!teardown.machines || !Array.isArray(teardown.machines.failures)) {
    throw new Error(`teardown must report Machine Partial Result, got ${JSON.stringify(teardown)}`);
  }
  const removed = (teardown.machines.successes || []).map((row) => row.machine_id);
  if (!removed.includes(workerMachine) && !(teardown.machines.failures || []).some((row) => row.machine_id === workerMachine)) {
    throw new Error(`worker Machine must be reported, got ${JSON.stringify(teardown)}`);
  }

  await client.close();
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
