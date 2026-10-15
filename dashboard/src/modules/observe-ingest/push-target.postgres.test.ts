import { it } from "@effect/vitest";
import type { Client } from "@ployz/sdk";
import { asc, eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import type { MachineId } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import { organizationMachine } from "#/modules/machines/tables";
import { deliverPushTargets } from "#/modules/observe-ingest/push-target.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f301";
const CURRENT = machineId("0");
const OLD = machineId("1");
const OFFLINE = machineId("2");
const url = (machine: string) => `http://localhost:3000/api/machines/${machine}/observe`;

type OnSet = (machine: string, republish: (machine: MachineId) => Promise<void>) => Promise<void>;

const setup = Effect.fn(function* (onSet: OnSet = async () => {}) {
  const dialed: Array<{ machine: string; url: string | null }> = [];
  const ployz = makePloyzLayer({
    connect: async (options) => {
      const [connection] = options.connections;
      const machine = connection && "machine_id" in connection ? connection.machine_id ?? "" : "";
      if (machine === OFFLINE) throw new Error("Endpoint unavailable");
      return asTestDouble<Client>()({
        setPushTarget: async (target: string | null) => {
          dialed.push({ machine, url: target });
          // What an old daemon's SDK refusal carries (`name_refusal`).
          if (machine === OLD) throw { code: "unsupported", message: "ployz.machine.set-push-target.v1 is not advertised", details: { capability: "ployz.machine.set-push-target.v1", target: machine } };
          await onSet(machine, republish);
        },
        close: async () => undefined,
      });
    },
  });
  const cloud = yield* Layer.build(Layer.merge(yield* storeTestCloud(), ployz));
  const provide = <A, E>(effect: Effect.Effect<A, E, Database | SecretEncryption>) => effect.pipe(Effect.provide(cloud), Effect.orDie);
  yield* provide(seedStoreOrganization(ORGANIZATION));
  yield* provide(enrollStoreServer(ORGANIZATION));
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    for (const machine of [OLD, OFFLINE]) {
      yield* drizzle.insert(organizationMachine).values({
        organizationId: ORGANIZATION, machineId: machine, clusterKey: "e".repeat(64),
        encryptedCapability: encryption.encrypt(`ployz1:cloud:${machine}`), isDialEntry: false,
      });
    }
  }));
  const confirmed = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ machineId: organizationMachine.machineId, pushTarget: organizationMachine.pushTarget })
      .from(organizationMachine).orderBy(asc(organizationMachine.machineId));
  }));
  const republish = (machine: MachineId) => Effect.runPromise(provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    yield* drizzle.update(organizationMachine)
      .set({ encryptedCapability: encryption.encrypt(`ployz1:rotated:${machine}`), pushTarget: null })
      .where(eq(organizationMachine.machineId, machine));
  })));
  const sweep = () => deliverPushTargets().pipe(Effect.provide(cloud), Effect.orDie);
  return { dialed, confirmed, sweep, republish };
});

it.live("a sweep points each Machine at its observe URL and records only what the Machine confirmed", () => Effect.gen(function* () {
  const { dialed, confirmed, sweep } = yield* setup();
  const outcomes = yield* sweep();
  expect(outcomes).toEqual(expect.arrayContaining([
    { machineId: CURRENT, delivery: "set" },
    { machineId: OLD, delivery: "lacks" },
    { machineId: OFFLINE, delivery: "unreachable" },
  ]));
  expect(dialed).toEqual(expect.arrayContaining([{ machine: CURRENT, url: url(CURRENT) }, { machine: OLD, url: url(OLD) }]));
  expect(yield* confirmed()).toEqual([
    { machineId: CURRENT, pushTarget: url(CURRENT) },
    { machineId: OLD, pushTarget: null },
    { machineId: OFFLINE, pushTarget: null },
  ]);
}));

it.live("a confirmed Machine is not dialed again; an old or offline one is retried each sweep", () => Effect.gen(function* () {
  const { dialed, sweep } = yield* setup();
  yield* sweep();
  dialed.length = 0;
  const outcomes = yield* sweep();
  expect(outcomes.map(({ machineId }) => machineId).sort()).toEqual([OLD, OFFLINE].sort());
  expect(dialed).toEqual([{ machine: OLD, url: url(OLD) }]);
}));

it.live("a capability published while Cloud dials leaves the Machine pending for the next sweep", () => Effect.gen(function* () {
  let rotate = true;
  const { confirmed, sweep } = yield* setup(async (machine, republish) => {
    if (machine === CURRENT && rotate) {
      rotate = false;
      await republish(CURRENT);
    }
  });
  yield* sweep();
  expect((yield* confirmed()).find((row) => row.machineId === CURRENT)?.pushTarget).toBeNull();
  yield* sweep();
  expect((yield* confirmed()).find((row) => row.machineId === CURRENT)?.pushTarget).toBe(url(CURRENT));
}));
