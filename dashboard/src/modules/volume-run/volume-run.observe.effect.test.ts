import type { Client, Connection, RuntimeWatchView } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Exit, Fiber, Layer } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import { makeOrganizationRuntimeLayer, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { runtimeWatchFrameFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { Database } from "#/server/database.server";
import { noPairingChanges } from "#/test/organization-runtime";
import { observeVolumeMembers } from "./volume-run.server";

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
      Effect.exit,
      Effect.forkChild,
    );
    yield* Deferred.await(watching);
    yield* TestClock.adjust(RUNTIME_FRAME_TIMEOUT_MS);
    const exit = yield* Fiber.join(observing);
    const reason = Exit.isFailure(exit) ? exit.cause.reasons[0] : undefined;
    assert.isTrue(reason?._tag === "Fail" && reason.error instanceof RuntimeConnectionFailure && reason.error.waitable);
  }),
);
