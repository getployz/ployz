import type { Client, ConfigStore, Connection, MachineId } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Exit, Fiber, Layer } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import { planClean, planRemove, UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import {
  CLI_CLUSTER_DEADLINE_MS,
  makeOrganizationRuntimeLayer,
  ORGANIZATION_CONNECT_TIMEOUT,
  withClusterDeadline,
} from "#/modules/runtime/organization-runtime.server";
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

// No Environment owns any Namespace, so every clean reaches the Cluster.
const noOwners = Layer.succeed(CloudStore, {
  open: Effect.succeed(asTestDouble<ConfigStore>()({ read: async () => ({ namespaces: [] }) })),
});

const settle = Effect.promise(() => new Promise((resolve) => setTimeout(resolve, 0)));
const unanswered = (exit: Exit.Exit<unknown, unknown> | undefined) =>
  exit !== undefined && Exit.isSuccess(exit) && exit.value instanceof Conflict && exit.value.message === UNREACHABLE_MESSAGE;

it.effect("a clean whose Store read takes 45 seconds and whose data loss never comes back refuses at 50, before the CLI gives up at 60", () =>
  Effect.gen(function* () {
    const reading = yield* Deferred.make<void>();
    const asking = yield* Deferred.make<void>();
    let read: () => void = () => {};
    const slowStore = Layer.succeed(CloudStore, {
      open: Effect.succeed(asTestDouble<ConfigStore>()({
        read: () => new Promise((resolve) => {
          read = () => resolve({ namespaces: [] });
          Effect.runSync(Deferred.succeed(reading, undefined));
        }),
      })),
    });
    const layer = makeOrganizationRuntimeLayer(
      () => Effect.succeed({ kind: "ready", generation: "current", connections }),
      noPairingChanges,
    ).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({
        close: async () => {},
        dataLossIfNamespaceDestroyed: () => {
          Effect.runSync(Deferred.succeed(asking, undefined));
          return new Promise(() => {});
        },
        runtime: { watch: async function* () { yield frame; } },
      }),
    })), Layer.merge(slowStore));
    // The CLI handler starts the clock when the request comes in, before the Store read.
    const planning = yield* planClean("org-1", "stray").pipe(
      withClusterDeadline(CLI_CLUSTER_DEADLINE_MS), Effect.provide(layer), Effect.flip, Effect.forkChild,
    );
    yield* Deferred.await(reading);
    yield* TestClock.adjust("45 seconds");
    read();
    yield* Deferred.await(asking);
    yield* TestClock.adjust("5 seconds");
    // The test clock stands still from here, so a plan that settles at all settled by 50 seconds.
    assert.isTrue(unanswered(Exit.succeed(yield* Fiber.join(planning))));
  }),
);

it.effect("a connections query held past the deadline is not interrupted, and the clean then refuses without dialing", () =>
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
    ).pipe(Layer.provide(makePloyzLayer({
      connect: async () => {
        dials += 1;
        return client;
      },
    })), Layer.merge(noOwners));
    const planning = yield* planClean("org-1", "stray").pipe(
      withClusterDeadline(CLI_CLUSTER_DEADLINE_MS), Effect.provide(layer), Effect.flip, Effect.forkChild,
    );
    yield* Deferred.await(querying);
    yield* TestClock.adjust("55 seconds");
    yield* settle;
    assert.isFalse(interrupted);
    assert.isUndefined(planning.pollUnsafe());
    yield* Deferred.succeed(answer, undefined);
    assert.isTrue(unanswered(Exit.succeed(yield* Fiber.join(planning))));
    assert.strictEqual(dials, 0);
  }),
);
