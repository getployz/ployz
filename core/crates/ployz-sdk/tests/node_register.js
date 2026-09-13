"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const expectRpcError = require("./expect-rpc-error");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
const socketDirectory = process.env.PLOYZ_SOCKET_DIRECTORY;
const connectionsFor = (id) => [{ unix: path.join(socketDirectory, `${id}.sock`) }];
const machineId = process.env.PLOYZ_MACHINE_ID;
const isolatedMachineId = process.env.PLOYZ_ISOLATED_MACHINE_ID;
const unknownMachineId = process.env.PLOYZ_UNKNOWN_MACHINE_ID;

if (
  !addon ||
  !pkg ||
  !socketDirectory ||
  !machineId ||
  !isolatedMachineId ||
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
    machine_id: "11111111111111111111111111111111",
    assigned_subnet: "10.210.1.0/24",
    name: "joiner",
    initial_policy: {
      labels: {},
      accepts_builds: true,
      accepts_services: true,
      accepts_ingress: true,
    },
    storage: "zfs",
    public_key: Array(32).fill(1),
    advertised_endpoints: ["192.0.2.9:51820"],
  };
}

(async () => {
  const client = await sdk.connect({ connections: connectionsFor(machineId) });
  const assignment = sdk.allocateEnrollment(joinerIdentity(), await client.observeEnrollment(), []);
  const registered = await client.register(assignment);
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

  const again = await client.register(assignment);
  if (again.assigned_machine.name !== "joiner") {
    throw new Error("second register must reuse the saved assignment");
  }

  const isolatedClient = await sdk.connect({ connections: connectionsFor(isolatedMachineId) });
  const isolated = await expectRpc(() => isolatedClient.register(assignment), "unavailable");
  await isolatedClient.close();
  if (isolated.message !== "this Machine is isolation-locked") {
    throw new Error(`expected isolation lock, got ${isolated.message}`);
  }

  await expectRpc(
    () =>
      sdk.connect({ connections: connectionsFor(unknownMachineId) }),
    "internal",
  );
  await expectRpc(
    () => client.register({ not: "EnrollmentAssignment" }),
    "invalid_argument",
  );
  await expectRpc(
    () =>
      client.register({
        ...assignment,
        request: { ...assignment.request, storage: "other" },
      }),
    "invalid_argument",
  );

  if (typeof sdk.Client.prototype.register !== "function") {
    throw new Error("Client.register must exist");
  }
  if (Object.hasOwn(sdk, "connectHeld")) {
    throw new Error("connectHeld must not be exported");
  }
  for (const obsolete of ["register", "observeEnrollment", "publishEnrollment", "listHeld", "revokePairing"]) {
    if (Object.hasOwn(sdk, obsolete)) throw new Error(`${obsolete} must not be exported`);
  }
  await client.close();
  await expectRpc(() => client.observeEnrollment(), "unavailable");
  await expectRpc(() => client.register(assignment), "unavailable");

  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
