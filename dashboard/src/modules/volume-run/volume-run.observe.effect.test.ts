import type { Client, Connection, RuntimeWatchView } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { vi } from "vitest";
import { Deferred, Effect, Fiber, Layer, Queue } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { requirements } from "#/modules/runtime/daemon-compat";
import { makeOrganizationRuntimeLayer, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { Database, type DatabaseService } from "#/server/database.server";
import { noPairingChanges } from "#/test/organization-runtime";
import { COPIES } from "./volume-run.test-fixture";
import { observeVolumeMembers, planOnCluster } from "./volume-run.server";

const connections: Connection[] = [{ management: "ployz1:preferred" }];

/** A Cluster whose first Runtime Watch frame comes once `framed` is resolved. */
const cluster = (framed: Promise<void>, watching: Deferred.Deferred<void>) => makeOrganizationRuntimeLayer(
  () => Effect.succeed({ kind: "ready", generation: "current", connections }),
  noPairingChanges,
).pipe(
  Layer.provide(makePloyzLayer({
    connect: async () => asTestDouble<Client>()({
      close: async () => {},
      runtime: {
        watch: async function* (): AsyncGenerator<RuntimeWatchView> {
          Effect.runSync(Deferred.succeed(watching, undefined));
          await framed;
          yield runtimeWatchFrameFixture();
        },
      },
    }),
  })),
  // Only a Server that lacks the verb reads its release channel from the database.
  Layer.merge(Layer.succeed(Database, undefined as never)),
);

it.effect("a Cluster whose first frame comes after 6 seconds is observed, not reported as not answering", () =>
  Effect.gen(function* () {
    const watching = yield* Deferred.make<void>();
    let frame: () => void = () => {};
    const framed = new Promise<void>((resolve) => { frame = resolve; });
    const observing = yield* observeVolumeMembers("org-1", "pg-data").pipe(Effect.provide(cluster(framed, watching)), Effect.forkChild);
    yield* Deferred.await(watching);
    yield* TestClock.adjust("6 seconds");
    frame();
    assert.deepStrictEqual(yield* Fiber.join(observing), []);
  }),
);

it.effect("a Cluster that never sends a frame fails as one to wait out once the frame budget runs out", () =>
  Effect.gen(function* () {
    const watching = yield* Deferred.make<void>();
    const observing = yield* observeVolumeMembers("org-1", "pg-data").pipe(
      Effect.provide(cluster(new Promise<void>(() => {}), watching)),
      Effect.flip,
      Effect.forkChild,
    );
    yield* Deferred.await(watching);
    yield* TestClock.adjust(RUNTIME_FRAME_TIMEOUT_MS);
    const error = yield* Fiber.join(observing);
    assert.isTrue(error instanceof RuntimeConnectionFailure && error.waitable);
  }),
);

const volume = { environmentId: "e1", volumeName: "data", dockerVolume: "production_data", refquotaBytes: 0, recordedBytes: 0 };
const release = { volumeId: "v1", environment: { project: "web", environment: "production" }, kind: "release", args: {} } as const;
const unanswered = { ok: false, refusal: { code: "unavailable", message: "Your servers aren't answering. Try again once they are.", details: null } } as const;

// One Server holding only a mirror: a release reads the members, then which slot keeps the Service.
const machine = runtimeWatchMachineFixture("a".repeat(32), "box-1");
const slotOnly = runtimeWatchFrameFixture({
  machines: [runtimeWatchMachineObservationFixture({
    machine: { ...machine, public_key: Array.from({ length: 32 }, () => 1), runtime: { ...machine.runtime, capabilities: [requirements.volumeFloor.capability] } },
  })],
});

/**
 * A Cluster where each session's connections query, connect and first frame wait for the test to answer them, so it
 * can spend database and Cluster time apart.
 */
const handAnswered = Effect.gen(function* () {
  const queries = yield* Queue.unbounded<() => void>();
  const dials = yield* Queue.unbounded<() => void>();
  const watches = yield* Queue.unbounded<() => void>();
  let interrupted = 0;
  const layer = makeOrganizationRuntimeLayer(
    () => Effect.callback<void>((resume) => { Queue.offerUnsafe(queries, () => resume(Effect.void)); }).pipe(
      Effect.onInterrupt(() => Effect.sync(() => { interrupted += 1; })),
      Effect.as({ kind: "ready" as const, generation: "current", connections }),
    ),
    noPairingChanges,
  ).pipe(
    Layer.provide(makePloyzLayer({
      connect: () => new Promise<Client>((resolve) => {
        Queue.offerUnsafe(dials, () => resolve(asTestDouble<Client>()({
          close: async () => {},
          volumeSwitch: async () => ({ copy: COPIES.mirror }),
          runtime: {
            watch: async function* (): AsyncGenerator<RuntimeWatchView> {
              await new Promise<void>((frame) => { Queue.offerUnsafe(watches, frame); });
              yield slotOnly;
            },
          },
        })));
      }),
    })),
    // With no commitment owed and every Server current, the plan reads neither the Store nor the database.
    Layer.merge(Layer.succeed(CloudStore, { open: Effect.die("the plan read the Store") })),
    Layer.merge(Layer.succeed(Database, undefined as never)),
  );
  return { layer, queries, dials, watches, interrupted: () => interrupted };
});

it.effect("a release whose members read takes 35 seconds and whose next connect never answers is refused at 45 seconds of Cluster time", () =>
  Effect.gen(function* () {
    const cluster = yield* handAnswered;
    const planning = yield* planOnCluster("org-1", volume, release, null).pipe(Effect.provide(cluster.layer), Effect.forkChild);
    (yield* Queue.take(cluster.queries))();
    // The members read connects after 28 seconds and frames 7 seconds later.
    const connectMembers = yield* Queue.take(cluster.dials);
    yield* TestClock.adjust("28 seconds");
    connectMembers();
    const frameMembers = yield* Queue.take(cluster.watches);
    yield* TestClock.adjust("7 seconds");
    frameMembers();
    // The keptOn read dials and never connects; its own ceiling is 30 seconds, the budget has 10 left.
    (yield* Queue.take(cluster.queries))();
    yield* Queue.take(cluster.dials);
    yield* TestClock.adjust("10 seconds");
    // The test clock stands still from here, so a plan that settles at all settled by 45 seconds of Cluster time.
    assert.deepStrictEqual(yield* Fiber.join(planning), unanswered);
  }),
);

/**
 * One Server that lacks the generation floor and holds a mirror, so a release names what it lacks; its release channel
 * query waits for the test to answer it, and its inspect answers once `inspected` resolves.
 */
const lacking = Effect.gen(function* () {
  const queries = yield* Queue.unbounded<() => void>();
  const inspects = yield* Queue.unbounded<void>();
  let interrupted = 0;
  let finished = 0;
  const releaseChannel = Effect.callback<void>((resume) => { Queue.offerUnsafe(queries, () => resume(Effect.void)); }).pipe(
    Effect.tap(() => Effect.sync(() => { finished += 1; })),
    Effect.onInterrupt(() => Effect.sync(() => { interrupted += 1; })),
    Effect.as([]),
  );
  const floorless = runtimeWatchFrameFixture({
    machines: [runtimeWatchMachineObservationFixture({ machine: { ...machine, runtime: { ...machine.runtime, capabilities: ["volume_move"] } } })],
  });
  const layer = (inspected: Promise<void>) => makeOrganizationRuntimeLayer(
    () => Effect.succeed({ kind: "ready", generation: "current", connections }),
    noPairingChanges,
  ).pipe(
    Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({
        close: async () => {},
        volumeSwitch: async () => {
          Queue.offerUnsafe(inspects, undefined);
          await inspected;
          return { copy: COPIES.mirror };
        },
        runtime: {
          watch: async function* (): AsyncGenerator<RuntimeWatchView> {
            yield floorless;
          },
        },
      }),
    })),
    Layer.merge(Layer.succeed(Database, asTestDouble<DatabaseService>()({
      drizzle: asTestDouble<DatabaseService["drizzle"]>()({ select: () => ({ from: () => ({ where: () => releaseChannel }) }) }),
    }))),
  );
  return { layer, queries, inspects, interrupted: () => interrupted, finished: () => finished };
});

it.effect("a Server's release channel query that takes 12 seconds finishes, and the member names the release it needs", () =>
  Effect.gen(function* () {
    vi.stubGlobal("fetch", async () => new Response("v0.2.0\n"));
    const cluster = yield* lacking;
    const observing = yield* observeVolumeMembers("org-1", "pg-data", release).pipe(Effect.provide(cluster.layer(Promise.resolve())), Effect.forkChild);
    const query = yield* Queue.take(cluster.queries);
    yield* TestClock.adjust("12 seconds");
    query();
    assert.deepStrictEqual(yield* Fiber.join(observing), [{
      machine: { id: machine.id, name: machine.name },
      answered: false,
      lack: "Volume move needs a newer Ployz on box-1. 0.2.0 is out.",
      outdated: true,
    }]);
    assert.strictEqual(cluster.finished(), 1);
    assert.strictEqual(cluster.interrupted(), 0);
  }).pipe(Effect.ensuring(Effect.sync(() => vi.unstubAllGlobals()))),
);

it.effect("a Server's lack claims no newer Ployz when its release can't be looked up", () =>
  Effect.gen(function* () {
    vi.stubGlobal("fetch", async () => { throw new Error("offline"); });
    // Past the release pointer cache, so an earlier test's release isn't read back.
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(Date.now() + 10 * 60_000);
    const cluster = yield* lacking;
    const observing = yield* observeVolumeMembers("org-1", "pg-data", release).pipe(Effect.provide(cluster.layer(Promise.resolve())), Effect.forkChild);
    (yield* Queue.take(cluster.queries))();
    assert.deepStrictEqual(yield* Fiber.join(observing), [{
      machine: { id: machine.id, name: machine.name },
      answered: false,
      lack: `Volume move is not in Ployz ${machine.runtime.daemon_version} on box-1.`,
    }]);
  }).pipe(Effect.ensuring(Effect.sync(() => { vi.useRealTimers(); vi.unstubAllGlobals(); }))),
);

it.effect("a Server whose inspect never answers is a member that didn't answer after 10 seconds", () =>
  Effect.gen(function* () {
    const cluster = yield* lacking;
    const observing = yield* observeVolumeMembers("org-1", "pg-data", release).pipe(
      Effect.provide(cluster.layer(new Promise<void>(() => {}))),
      Effect.forkChild,
    );
    yield* Queue.take(cluster.inspects);
    yield* TestClock.adjust("10 seconds");
    assert.deepStrictEqual(yield* Fiber.join(observing), [{ machine: { id: machine.id, name: machine.name }, answered: false }]);
  }),
);

it.effect("database reads between a plan's Cluster reads are never interrupted and spend none of its Cluster time", () =>
  Effect.gen(function* () {
    const cluster = yield* handAnswered;
    const planning = yield* planOnCluster("org-1", volume, release, null).pipe(Effect.provide(cluster.layer), Effect.forkChild);
    // The members session's connections query takes 20 seconds, then its Cluster read takes 35.
    const membersQuery = yield* Queue.take(cluster.queries);
    yield* TestClock.adjust("20 seconds");
    membersQuery();
    const connectMembers = yield* Queue.take(cluster.dials);
    yield* TestClock.adjust("28 seconds");
    connectMembers();
    const frameMembers = yield* Queue.take(cluster.watches);
    yield* TestClock.adjust("7 seconds");
    frameMembers();
    // The keptOn session's query takes 30 seconds more, then its Cluster read 8: 43 seconds of Cluster time in all.
    const keptQuery = yield* Queue.take(cluster.queries);
    yield* TestClock.adjust("30 seconds");
    keptQuery();
    const connectKept = yield* Queue.take(cluster.dials);
    yield* TestClock.adjust("8 seconds");
    connectKept();
    (yield* Queue.take(cluster.watches))();
    const plan = yield* Fiber.join(planning);
    assert.strictEqual(cluster.interrupted(), 0);
    assert.isTrue(plan.ok);
  }),
);
