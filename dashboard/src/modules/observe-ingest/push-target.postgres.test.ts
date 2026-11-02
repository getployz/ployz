import { it } from "@effect/vitest";
import type { Client } from "@ployz/sdk";
import { asc, eq } from "drizzle-orm";
import { Effect } from "effect";
import { assert, expect } from "vitest";
import type { MachineId } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import { clusterSealingKey, organizationMachine } from "#/modules/machines/tables";
import { deliverPushTargets } from "#/modules/observe-ingest/push-target.server";
import { SESSION_CLOSE_CLUSTER_MS } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { machineId } from "#/test/ids";
import { connectionAborts } from "#/test/connection-aborts";
import { enrollStoreServer, seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f301";
const CURRENT = machineId("0");
const OLD = machineId("1");
const OFFLINE = machineId("2");
const SEALING_KEY = `${"K".repeat(43)}=`;
const url = (machine: string) => `http://localhost:3000/api/machines/${machine}/observe`;

type OnSet = (machine: string, republish: (machine: MachineId) => Promise<void>) => Promise<void>;

const setup = Effect.fn(function* (onSet: OnSet = async () => {}, close: () => Promise<void> = async () => undefined, foreign: string[] = []) {
  const dialed: Array<{ machine: string; url: string | null }> = [];
  const expected = new Map<string, string | null>();
  const aborts = connectionAborts();
  const ployz = makePloyzLayer({
    connect: async (options) => {
      aborts.watch(options.signal);
      const [connection] = options.connections;
      const machine = connection && "machine_id" in connection ? connection.machine_id ?? "" : "";
      expected.set(machine, options.expect?.sealingPublicKey ?? null);
      if (machine === OFFLINE) throw new Error("Endpoint unavailable");
      // A `foreign` Machine holds another Cluster's key; every other one holds none yet.
      if (foreign.includes(machine) && options.expect) throw { code: "unauthenticated", message: `Machine ${machine} belongs to another Cluster`, details: null };
      if (options.expect && options.expect.allowUnverified !== true) throw { code: "unauthenticated", message: `Machine ${machine} can't show which Cluster it belongs to`, details: null };
      return asTestDouble<Client>()({
        setPushTarget: async (target: string | null) => {
          dialed.push({ machine, url: target });
          // What an old daemon's SDK refusal carries (`name_refusal`).
          if (machine === OLD) throw { code: "unsupported", message: "ployz.machine.set-push-target.v1 is not advertised", details: { capability: "ployz.machine.set-push-target.v1", target: machine } };
          await onSet(machine, republish);
        },
        close,
      });
    },
  });
  const cloud = yield* storeTestCloudWith(ployz);
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
  // The current pairing records its Cluster's key; the old rows' pairing records none.
  const recordKey = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [current = assert.fail("no current Machine")] = yield* drizzle.select({ clusterKey: organizationMachine.clusterKey }).from(organizationMachine)
      .where(eq(organizationMachine.machineId, CURRENT));
    yield* drizzle.insert(clusterSealingKey).values({ organizationId: ORGANIZATION, clusterKey: current.clusterKey, publicKey: SEALING_KEY });
  }));
  return { dialed, expected, aborts, confirmed, sweep, republish, recordKey };
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

it.live("a sweep finishes within its Cluster time, and aborts each connection, when a Machine's session never closes", () => Effect.gen(function* () {
  const { aborts, confirmed, sweep } = yield* setup(undefined, () => new Promise<void>(() => {}));
  const outcomes = yield* sweep().pipe(Effect.timeout(SESSION_CLOSE_CLUSTER_MS + 8_000));
  expect(outcomes).toEqual(expect.arrayContaining([{ machineId: CURRENT, delivery: "set" }, { machineId: OLD, delivery: "lacks" }]));
  expect((yield* confirmed()).find((row) => row.machineId === CURRENT)?.pushTarget).toBe(url(CURRENT));
  expect(aborts.aborted()).toEqual([true, true, true]);
}), 30_000);

it.live("each Machine is dialed expecting its own pairing's recorded key, and one answering for another Cluster stays pending", () => Effect.gen(function* () {
  const { expected, confirmed, sweep, recordKey } = yield* setup(undefined, undefined, [CURRENT]);
  yield* recordKey();
  const outcomes = yield* sweep();
  expect(expected.get(CURRENT)).toBe(SEALING_KEY);
  expect(expected.get(OLD)).toBeNull();
  expect(outcomes).toEqual(expect.arrayContaining([{ machineId: CURRENT, delivery: "unreachable" }]));
  expect((yield* confirmed()).find((row) => row.machineId === CURRENT)?.pushTarget).toBeNull();
}));

it.live("a Machine that holds no key yet is still pointed here once its pairing records one", () => Effect.gen(function* () {
  const { expected, confirmed, sweep, recordKey } = yield* setup();
  yield* recordKey();
  yield* sweep();
  expect(expected.get(CURRENT)).toBe(SEALING_KEY);
  expect((yield* confirmed()).find((row) => row.machineId === CURRENT)?.pushTarget).toBe(url(CURRENT));
}));
