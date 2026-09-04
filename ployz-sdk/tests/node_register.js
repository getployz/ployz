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
const notQuietMachineId = process.env.PLOYZ_NOT_QUIET_MACHINE_ID;
const unknownMachineId = process.env.PLOYZ_UNKNOWN_MACHINE_ID;

if (
  !addon ||
  !pkg ||
  !relayUrl ||
  !bearer ||
  !pairing ||
  !machineId ||
  !notQuietMachineId ||
  !unknownMachineId
) {
  throw new Error("Node register smoke is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-register-"));
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
    return rpc;
  }
}

function joinerIdentity() {
  return {
    name: "joiner",
    storage: "zfs",
    public_key: Array(32).fill(1),
    advertised_endpoints: ["192.0.2.9:51820"],
  };
}

(async () => {
  const held = await sdk.listHeld(relayUrl, bearer, pairing);
  const target = held.find((row) => row.machineId === machineId);
  if (!target) {
    throw new Error(`listHeld missing Dial target ${machineId}: ${JSON.stringify(held)}`);
  }

  const registered = await sdk.register(
    relayUrl,
    bearer,
    pairing,
    target.machineId,
    joinerIdentity(),
  );
  if (!registered || !registered.assigned_machine) {
    throw new Error(`expected Registered, got ${JSON.stringify(registered)}`);
  }
  if (registered.assigned_machine.name !== "joiner") {
    throw new Error(
      `assigned Machine Name must echo identity, got ${registered.assigned_machine.name}`,
    );
  }
  if (!Array.isArray(registered.visible_peers)) {
    throw new Error("Registered.visible_peers must be an array");
  }

  const again = await sdk.register(
    relayUrl,
    bearer,
    pairing,
    target.machineId,
    joinerIdentity(),
  );
  if (again.assigned_machine.name !== "joiner") {
    throw new Error("second register must reuse a closed Dial");
  }

  const notQuiet = await expectRpc(
    () =>
      sdk.register(relayUrl, bearer, pairing, notQuietMachineId, joinerIdentity()),
    "unavailable",
  );
  if (notQuiet.message !== "Allocator is not quiet") {
    throw new Error(`expected Allocator not-quiet, got ${notQuiet.message}`);
  }

  await expectRpc(
    () => sdk.register(relayUrl, "wrong-secret", pairing, machineId, joinerIdentity()),
    "unauthenticated",
  );
  await expectRpc(
    () => sdk.register(relayUrl, bearer, "", machineId, joinerIdentity()),
    "invalid_argument",
  );
  await expectRpc(
    () =>
      sdk.register(relayUrl, bearer, pairing, unknownMachineId, joinerIdentity()),
    "not_found",
  );
  await expectRpc(
    () => sdk.register(relayUrl, bearer, pairing, machineId, { not: "RegisterRequest" }),
    "invalid_argument",
  );
  await expectRpc(
    () =>
      sdk.register(relayUrl, bearer, pairing, machineId, {
        ...joinerIdentity(),
        storage: "other",
      }),
    "invalid_argument",
  );

  if (typeof sdk.Client.prototype.register !== "undefined") {
    throw new Error("Client.register must not exist");
  }
  if (Object.hasOwn(sdk, "connectHeld")) {
    throw new Error("connectHeld must not be exported");
  }
  if (typeof sdk.register !== "function") {
    throw new Error("register must be exported");
  }

  const client = await sdk.connect({ relayUrl, bearer, pairing, machineId });
  if (typeof client.register !== "undefined") {
    throw new Error("operator Client must not grow register");
  }
  await client.close();

  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
