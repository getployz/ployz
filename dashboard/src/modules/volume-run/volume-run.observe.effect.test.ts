import type { Client, Connection, RuntimeWatchView } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Exit, Fiber, Layer, Queue } from "effect";
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

it.effect("a release whose two reads each take 35 seconds is reported as not answering at 45 seconds, inside the CLI's request", () =>
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
    const volume = { environmentId: "e1", volumeName: "data", dockerVolume: "production_data", refquotaBytes: 0 };
    const planning = yield* planOnCluster("org-1", volume, {
      volumeId: "v1", environment: { project: "web", environment: "production" }, kind: "release", args: {},
    }, null).pipe(Effect.provide(layer), Effect.forkChild);
    // The members read connects after 28 seconds and frames 7 seconds later.
    const connectMembers = yield* Queue.take(dials);
    yield* TestClock.adjust("28 seconds");
    connectMembers();
    const frameMembers = yield* Queue.take(watches);
    yield* TestClock.adjust("7 seconds");
    frameMembers();
    // The keptOn read dials at 35 seconds; the deadline ends it at 45, before its own 35 seconds are up.
    yield* Queue.take(dials);
    yield* TestClock.adjust("10 seconds");
    yield* Effect.promise(() => new Promise((resolve) => setTimeout(resolve, 0)));
    const settled = planning.pollUnsafe();
    assert.deepStrictEqual(settled, Exit.succeed({
      ok: false, refusal: { code: "unavailable", message: "Your servers aren't answering. Try again once they are.", details: null },
    }));
  }),
);
