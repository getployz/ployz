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
const loadedMachine = process.env.PLOYZ_LOADED_MACHINE;
const loadedMachineId = process.env.PLOYZ_LOADED_MACHINE_ID;
const emptyMachine = process.env.PLOYZ_EMPTY_MACHINE;

if (
  !addon ||
  !pkg ||
  !relayUrl ||
  !bearer ||
  !pairing ||
  !machineId ||
  !loadedMachine ||
  !loadedMachineId ||
  !emptyMachine
) {
  throw new Error("Node Data Loss is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-data-loss-"));
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
  const client = await sdk.connect({ relayUrl, bearer, pairing, machineId });

  const loaded = await client.dataLossIfMachineRemoved(loadedMachine);
  if (!Array.isArray(loaded.data_loss)) {
    throw new Error("ObservedDataLoss.data_loss must be an array");
  }
  if (loaded.data_loss.length !== 2) {
    throw new Error(
      `expected two Data Loss entries, got ${JSON.stringify(loaded)}`,
    );
  }
  const names = loaded.data_loss
    .map((row) => dockerVolume(row).name)
    .slice()
    .sort();
  if (names.join(",") !== "data,logs") {
    throw new Error(`unexpected Docker Volume names: ${names.join(",")}`);
  }
  for (const row of loaded.data_loss) {
    const id = dockerVolume(row);
    if (id.machine_id !== loadedMachineId) {
      throw new Error(
        `Docker Volume Data Loss must carry its Machine, got ${id.machine_id}`,
      );
    }
  }

  const none = await client.dataLossIfMachineRemoved(emptyMachine);
  if (!Array.isArray(none.data_loss) || none.data_loss.length !== 0) {
    throw new Error(`expected no Data Loss, got ${JSON.stringify(none)}`);
  }

  const again = await client.dataLossIfMachineRemoved(loadedMachine);
  if (JSON.stringify(again) !== JSON.stringify(loaded)) {
    throw new Error("the read must be stable when the operator cancels");
  }

  await client.close();
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
