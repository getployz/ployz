import type { Client, Connection, RuntimeWatchView } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Fiber, Layer, Queue } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { requirements } from "#/modules/runtime/daemon-compat";
import {
  CLI_CLUSTER_DEADLINE_MS,
  makeOrganizationRuntimeLayer,
  RUNTIME_FRAME_TIMEOUT_MS,
  withClusterDeadline,
} from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { Database } from "#/server/database.server";
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

const volume = { environmentId: "e1", volumeName: "data", dockerVolume: "production_data", refquotaBytes: 0 };
const release = { volumeId: "v1", environment: { project: "web", environment: "production" }, kind: "release", args: {} } as const;
const unanswered = { ok: false, refusal: { code: "unavailable", message: "Your servers aren't answering. Try again once they are.", details: null } } as const;

it.effect("a release whose two reads each take 35 seconds is reported as not answering at the request's 50-second deadline", () =>
  Effect.gen(function* () {
    // One Server holding only a mirror: the release reads the members, then which slot keeps the Service.
    const machine = runtimeWatchMachineFixture("a".repeat(32), "box-1");
    const slotOnly = runtimeWatchFrameFixture({
      machines: [runtimeWatchMachineObservationFixture({
        machine: { ...machine, public_key: Array.from({ length: 32 }, () => 1), runtime: { ...machine.runtime, capabilities: [requirements.volumeFloor.capability] } },
      })],
    });
    // Each read dials a fresh connection; the test answers its connect and its frame by hand.
    const dials = yield* Queue.unbounded<() => void>();
    const watches = yield* Queue.unbounded<() => void>();
    const layer = makeOrganizationRuntimeLayer(
      () => Effect.succeed({ kind: "ready", generation: "current", connections }),
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
    const planning = yield* planOnCluster("org-1", volume, release, null).pipe(
      withClusterDeadline(CLI_CLUSTER_DEADLINE_MS), Effect.provide(layer), Effect.forkChild,
    );
    // The members read connects after 28 seconds and frames 7 seconds later.
    const connectMembers = yield* Queue.take(dials);
    yield* TestClock.adjust("28 seconds");
    connectMembers();
    const frameMembers = yield* Queue.take(watches);
    yield* TestClock.adjust("7 seconds");
    frameMembers();
    // The keptOn read dials at 35 seconds; the deadline ends it at 50, before its own 35 seconds are up.
    yield* Queue.take(dials);
    yield* TestClock.adjust("15 seconds");
    // The test clock stands still from here, so a plan that settles at all settled by 50 seconds.
    assert.deepStrictEqual(yield* Fiber.join(planning), unanswered);
  }),
);

it.effect("a connections query held past the deadline is not interrupted, and the plan then refuses without dialing", () =>
  Effect.gen(function* () {
    const querying = yield* Deferred.make<void>();
    const answer = yield* Deferred.make<void>();
    let interrupted = false;
    let dials = 0;
    const layer = makeOrganizationRuntimeLayer(
      () => Deferred.succeed(querying, undefined).pipe(
        Effect.andThen(Deferred.await(answer)),
        Effect.onInterrupt(() => Effect.sync(() => { interrupted = true; })),
        Effect.as({ kind: "ready" as const, generation: "current", connections }),
      ),
      noPairingChanges,
    ).pipe(
      Layer.provide(makePloyzLayer({
        connect: async () => {
          dials += 1;
          return asTestDouble<Client>()({ close: async () => {} });
        },
      })),
      Layer.merge(Layer.succeed(CloudStore, { open: Effect.die("the plan read the Store") })),
      Layer.merge(Layer.succeed(Database, undefined as never)),
    );
    const planning = yield* planOnCluster("org-1", volume, release, null).pipe(
      withClusterDeadline(CLI_CLUSTER_DEADLINE_MS), Effect.provide(layer), Effect.forkChild,
    );
    yield* Deferred.await(querying);
    yield* TestClock.adjust("55 seconds");
    yield* Effect.promise(() => new Promise((resolve) => setTimeout(resolve, 0)));
    assert.isFalse(interrupted);
    assert.isUndefined(planning.pollUnsafe());
    yield* Deferred.succeed(answer, undefined);
    assert.deepStrictEqual(yield* Fiber.join(planning), unanswered);
    assert.strictEqual(dials, 0);
  }),
);
