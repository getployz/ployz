import type { Client, Connection, MachineId } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Fiber, Layer } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import { planRemove, UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { makeOrganizationRuntimeLayer, ORGANIZATION_CONNECT_TIMEOUT } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { runtimeWatchFrameFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { noPairingChanges } from "#/test/organization-runtime";
import { Conflict } from "#/server/public-error";

const connections: Connection[] = [{ management: "ployz1:preferred" }];
const machineId = "machine-1" as MachineId;
const frame = runtimeWatchFrameFixture({
  machines: [asTestDouble<ReturnType<typeof runtimeWatchFrameFixture>["machines"][number]>()({ machine: { id: machineId, name: "box-1" } })],
});

const client = asTestDouble<Client>()({
  close: async () => {},
  runtime: { watch: async function* () { yield frame; } },
});

// A removal that confirms no Volumes reads no Store.
const noStore = Layer.succeed(CloudStore, { open: Effect.die("planRemove read the Store") });

const runtime = (connect: Parameters<typeof makePloyzLayer>[0]["connect"]) => makeOrganizationRuntimeLayer(
  () => Effect.succeed({ kind: "ready", generation: "current", connections }),
  noPairingChanges,
).pipe(Layer.provide(makePloyzLayer({ connect })), Layer.merge(noStore));

it.effect("a Server whose connection comes up after 6 seconds is planned, not reported as not answering", () =>
  Effect.gen(function* () {
    const dialing = yield* Deferred.make<void>();
    let connected: (client: Client) => void = () => {};
    const layer = runtime(() => new Promise<Client>((resolve) => {
      connected = resolve;
      Effect.runSync(Deferred.succeed(dialing, undefined));
    }));
    const planning = yield* planRemove("org-1", machineId, null).pipe(Effect.provide(layer), Effect.forkChild);
    yield* Deferred.await(dialing);
    yield* TestClock.adjust("6 seconds");
    connected(client);
    const plan = yield* Fiber.join(planning);
    assert.strictEqual(plan?.name, "box-1");
  }),
);

it.effect("Servers that refuse the connection are reported as not answering", () =>
  Effect.gen(function* () {
    const error = yield* planRemove("org-1", machineId, null).pipe(
      Effect.provide(runtime(async () => { throw new Error("connection refused"); })),
      Effect.flip,
    );
    assert.isTrue(error instanceof Conflict && error.message === UNREACHABLE_MESSAGE);
  }),
);

it.effect("Servers that never answer the connection are reported as not answering once the connect phase runs out", () =>
  Effect.gen(function* () {
    const dialing = yield* Deferred.make<void>();
    const layer = runtime((options) => new Promise<Client>((_resolve, reject) => {
      options.signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
      Effect.runSync(Deferred.succeed(dialing, undefined));
    }));
    const planning = yield* planRemove("org-1", machineId, null).pipe(Effect.provide(layer), Effect.flip, Effect.forkChild);
    yield* Deferred.await(dialing);
    yield* TestClock.adjust(ORGANIZATION_CONNECT_TIMEOUT);
    const error = yield* Fiber.join(planning);
    assert.isTrue(error instanceof Conflict && error.message === UNREACHABLE_MESSAGE);
  }),
);
