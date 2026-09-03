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
const volumeMachineId = process.env.PLOYZ_VOLUME_MACHINE_ID;

if (!addon || !pkg || !relayUrl || !bearer || !pairing || !machineId || !volumeMachineId) {
  throw new Error("Node Project destroy is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-destroy-project-"));
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
  if (!loss || typeof loss !== "object" || loss.kind !== "docker_volume" || !loss.id) {
    throw new Error(`expected docker_volume Data Loss, got ${JSON.stringify(loss)}`);
  }
  if ("scope" in loss || "name" in loss || "machine_id" in loss) {
    throw new Error(`Data Loss identity must nest under id, not spread beside kind: ${JSON.stringify(loss)}`);
  }
  return loss.id;
}

(async () => {
  if (typeof sdk.Client.prototype.destroyProject !== "function") {
    throw new Error("Client.destroyProject must be a method");
  }
  if (typeof sdk.Client.prototype.dataLossIfProjectDestroyed !== "function") {
    throw new Error("Client.dataLossIfProjectDestroyed must be a method");
  }
  if (typeof sdk.Client.prototype.confirmAll === "function") {
    throw new Error("no API may confirm a read's Data Loss without naming its entries");
  }

  const client = await sdk.connect({ relayUrl, bearer, pairing, machineId });

  const preserved = await client.dataLossIfProjectDestroyed("shop");
  if (!Array.isArray(preserved.data_loss) || preserved.data_loss.length !== 0) {
    throw new Error(`preserving volumes must be empty Data Loss, got ${JSON.stringify(preserved)}`);
  }

  const observed = await client.dataLossIfProjectDestroyed("shop", true);
  if (observed.data_loss.length !== 2) {
    throw new Error(`expected two Data Loss entries, got ${JSON.stringify(observed)}`);
  }
  const names = observed.data_loss.map((row) => dockerVolume(row).name).sort();
  if (names.join(",") !== "shop_data,shop_logs") {
    throw new Error(`unexpected Docker Volume names: ${names.join(",")}`);
  }
  for (const row of observed.data_loss) {
    const id = dockerVolume(row);
    if (id.machine_id !== volumeMachineId) {
      throw new Error(`Docker Volume Data Loss must carry its Machine, got ${id.machine_id}`);
    }
  }

  try {
    await client.destroyProject("shop", { confirmed: [] }, true);
    throw new Error("unconfirmed destroy must fail");
  } catch (error) {
    if (error.message === "unconfirmed destroy must fail") {
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
    if (missing.join(",") !== "shop_data,shop_logs") {
      throw new Error(`missing names mismatch: ${JSON.stringify(rpc)}`);
    }
  }

  try {
    await client.destroyProject("shop", observed, true);
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

  const reserved = await client.dataLossIfProjectDestroyed("ployz-system").then(
    () => {
      throw new Error("reserved Project Data Loss must fail");
    },
    (error) => parseRpc(error),
  );
  if (reserved.code !== "invalid_argument" || !reserved.message.includes("ployz-system")) {
    throw new Error(`reserved Project must be refused, got ${JSON.stringify(reserved)}`);
  }

  const union = {
    confirmed: [
      ...observed.data_loss,
      {
        kind: "docker_volume",
        id: { machine_id: volumeMachineId, name: "staging_data" },
      },
    ],
  };
  const destroyed = await client.destroyProject("shop", union, true);
  if (!destroyed || destroyed.type !== "success") {
    throw new Error(`expected successful destroy, got ${JSON.stringify(destroyed)}`);
  }

  const staging = await client.destroyProject("staging", union, true);
  if (!staging || staging.type !== "success") {
    throw new Error(`expected staging destroy to reuse the union, got ${JSON.stringify(staging)}`);
  }

  await client.close();
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
