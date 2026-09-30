import { createHash } from "node:crypto";
import { it } from "@effect/vitest";
import type { Client, Connection, MachineId } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { removeMachineActivity } from "#/modules/machines/machine-removal.server";
import { organizationMachine } from "#/modules/machines/tables";
import { OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e001";
const ENTRY = "0".repeat(32) as MachineId;
const OTHER = "1".repeat(32) as MachineId;

/** A Cluster that, like core, enters through the first Server that answers and refuses to remove that entry while another remains. */
const cluster = (servers: readonly MachineId[]) => makePloyzLayer({
  connect: async (options) => {
    const connections: readonly Connection[] = "connections" in options ? options.connections : [];
    const entry = connections[0]?.machine_id;
    return asTestDouble<Client>()({
      close: async () => undefined,
      removeMachine: async (machine: MachineId) => {
        if (machine === entry && servers.length > 1) {
          throw new Error("the current entry Machine cannot be removed while another Machine is visible");
        }
        return { reset_warning: null };
      },
    });
  },
});

const remove = (servers: readonly MachineId[], machineId: MachineId) => Effect.gen(function* () {
  const cloud = yield* Layer.build(yield* storeTestCloud());
  return yield* Effect.gen(function* () {
    yield* seedStoreOrganization(ORGANIZATION);
    yield* enrollStoreServer(ORGANIZATION);
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    for (const other of servers.filter((server) => server !== ENTRY)) {
      yield* drizzle.insert(organizationMachine).values({
        organizationId: ORGANIZATION, machineId: other, clusterKey: createHash("sha256").update("ppair_fixture_store").digest("hex"),
        encryptedCapability: encryption.encrypt(`ployz1:cloud:${other}`), isDialEntry: false,
      });
    }
    return yield* removeMachineActivity({ organizationId: ORGANIZATION, machineId, confirmDataLoss: [], noReset: false }).pipe(
      Effect.provide(OrganizationRuntimeLive.pipe(Layer.provide(Layer.merge(cluster(servers), Layer.succeedContext(cloud))))),
    );
  }).pipe(Effect.provide(cloud));
}).pipe(Effect.scoped);

it.live("removing the entry Server while another remains dials through the other", () => Effect.gen(function* () {
  expect(yield* remove([ENTRY, OTHER], ENTRY)).toMatchObject({ kind: "removed", resetWarning: null });
}), 60_000);

it.live("the last Server is removed through itself", () => Effect.gen(function* () {
  expect(yield* remove([ENTRY], ENTRY)).toMatchObject({ kind: "removed", resetWarning: null });
}), 60_000);
