import type { Client, Connection, MachineId } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Fiber, Layer, Queue, Stream } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import {
  makeOrganizationRuntimeLayer,
  ORGANIZATION_CONNECT_TIMEOUT,
  OrganizationRuntime,
} from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer, PloyzProviderError } from "#/modules/runtime/ployz.server";

const connections: Connection[] = [
  { management: "ployz1:preferred" },
  { management: "ployz1:spare" },
];

it.effect("passes ordered candidates to one SDK connection and finalizes the session", () =>
  Effect.gen(function* () {
    const dialed: unknown[] = [];
    let closed = 0;
    const ployz = makePloyzLayer({
      connect: async (options) => {
        assert.isTrue("connections" in options);
        if (!("connections" in options)) throw new Error("expected shared connector");
        dialed.push(options.connections);
        return asTestDouble<Client>()({
          close: async () => { closed += 1; },
        });
      },
    });
    const runtime = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "ready", generation: "grant-1", connections }),
    ).pipe(Layer.provide(ployz));

    yield* Effect.scoped(
      Effect.gen(function* () {
        const session = yield* (yield* OrganizationRuntime).open("org-1");
        assert.strictEqual(session.status, "connected");
        assert.deepStrictEqual(dialed, [connections]);
        assert.strictEqual(closed, 0);
      }),
    ).pipe(Effect.provide(runtime));
    assert.strictEqual(closed, 1);
  }),
);

it.effect("keeps missing pairing distinct from empty candidates without dialing", () =>
  Effect.gen(function* () {
    let dialed = 0;
    const ployz = makePloyzLayer({
      connect: async () => {
        dialed += 1;
        throw new Error("must not dial");
      },
    });
    const missing = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "missing" }),
    ).pipe(Layer.provide(ployz));
    const unreachable = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "ready", generation: "grant-1", connections: [] }),
    ).pipe(Layer.provide(ployz));
    const open = Effect.scoped(
      Effect.flatMap(OrganizationRuntime, (runtime) => runtime.open("org-1")),
    );

    assert.deepStrictEqual(yield* open.pipe(Effect.provide(missing)), { status: "no_connection" });
    assert.deepStrictEqual(yield* open.pipe(Effect.provide(unreachable)), { status: "unreachable", error: null });
    assert.strictEqual(dialed, 0);
  }),
);

it.effect("reports a saved single candidate as unreachable when SDK negotiation fails", () =>
  Effect.gen(function* () {
    const failure = new Error("intended Machine identity mismatch");
    let dialed = 0;
    const runtime = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "ready", generation: "grant-1", connections: connections.slice(0, 1) }),
    ).pipe(Layer.provide(makePloyzLayer({
      connect: async () => { dialed += 1; throw failure; },
    })));
    const session = yield* Effect.scoped(
      Effect.flatMap(OrganizationRuntime, (runtime) => runtime.open("org-1")),
    ).pipe(Effect.provide(runtime));
    assert.strictEqual(session.status, "unreachable");
    if (session.status !== "unreachable") throw new Error("expected unreachable");
    assert.instanceOf(session.error, PloyzProviderError);
    assert.strictEqual(session.error?.cause, failure);
    assert.strictEqual(dialed, 1);
  }),
);


it.effect("cancels only sessions of the removed organization and pairing generation", () =>
  Effect.gen(function* () {
    let closed = 0;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    })).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => { closed += 1; } }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      yield* service.open("org-1");
      yield* service.open("org-2");
      yield* service.cancel("org-1", "old");
      assert.strictEqual(closed, 0);
      yield* service.cancel("org-1", "current");
      assert.strictEqual(closed, 1);
      yield* service.cancel("org-1", "current");
      assert.strictEqual(closed, 1);
    })).pipe(Effect.provide(runtime));
    assert.strictEqual(closed, 2);
  }),
);

it.effect("removal during candidate load prevents dialing the removed generation", () =>
  Effect.gen(function* () {
    const loading = yield* Deferred.make<void>();
    const loaded = yield* Deferred.make<void>();
    let dialed = 0;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.gen(function* () {
      yield* Deferred.succeed(loading, undefined);
      yield* Deferred.await(loaded);
      return { kind: "ready" as const, generation: "current", connections };
    })).pipe(Layer.provide(makePloyzLayer({
      connect: async () => { dialed += 1; throw new Error("must not dial"); },
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      const opening = yield* service.open("org-1").pipe(Effect.forkChild);
      yield* Deferred.await(loading);
      yield* service.cancel("org-1", "current");
      yield* Deferred.succeed(loaded, undefined);
      assert.deepStrictEqual(yield* Fiber.join(opening), { status: "no_connection" });
      assert.strictEqual(dialed, 0);
    })).pipe(Effect.provide(runtime));
  }),
);

it.effect("removal aborts an in-progress SDK connection", () =>
  Effect.gen(function* () {
    const dialing = yield* Deferred.make<void>();
    let aborted = false;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    })).pipe(Layer.provide(makePloyzLayer({
      connect: (options) => new Promise<Client>((_resolve, reject) => {
        if (!("connections" in options)) throw new Error("expected shared connector");
        options.signal?.addEventListener("abort", () => {
          aborted = true;
          reject(new Error("aborted"));
        }, { once: true });
        Effect.runSync(Deferred.succeed(dialing, undefined));
      }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      const opening = yield* service.open("org-1").pipe(Effect.forkChild);
      yield* Deferred.await(dialing);
      yield* service.cancel("org-1", "current");
      assert.deepStrictEqual(yield* Fiber.join(opening), { status: "no_connection" });
      assert.isTrue(aborted);
    })).pipe(Effect.provide(runtime));
  }),
);

it.effect("notification stream cancels remote sessions and fails closed when disconnected", () =>
  Effect.gen(function* () {
    const notifications = yield* Queue.make<string, Error>();
    const closed = yield* Deferred.make<void>();
    const disconnected = yield* Deferred.make<void>();
    let count = 0;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    }), Effect.succeed(Stream.fromQueue(notifications))).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => {
        count += 1;
        Effect.runSync(Deferred.succeed(count === 1 ? closed : disconnected, undefined));
      } }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      yield* service.open("org-1");
      yield* Queue.offer(notifications, JSON.stringify({ organizationId: "org-1", generation: "current" }));
      yield* Deferred.await(closed);
      assert.strictEqual(count, 1);
      yield* service.open("org-2");
      yield* Queue.fail(notifications, new Error("connection lost"));
      yield* Deferred.await(disconnected);
      assert.strictEqual(count, 2);
      const result = yield* Effect.exit(service.open("org-3"));
      assert.strictEqual(result._tag, "Failure");
    })).pipe(Effect.provide(runtime));
  }),
);


it.effect("re-subscribes after the notification stream drops and resumes cancelling", () =>
  Effect.gen(function* () {
    const first = yield* Queue.make<string, Error>();
    const second = yield* Queue.make<string, Error>();
    const streams = [Stream.fromQueue(first), Stream.fromQueue(second)];
    let subscriptions = 0;
    const closedByDisconnect = yield* Deferred.make<void>();
    const closedByRemoval = yield* Deferred.make<void>();
    let count = 0;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    }), Effect.sync(() => {
      subscriptions += 1;
      const stream = streams[subscriptions - 1];
      if (stream === undefined) throw new Error("unexpected third subscription");
      return stream;
    })).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => {
        count += 1;
        Effect.runSync(Deferred.succeed(count === 1 ? closedByDisconnect : closedByRemoval, undefined));
      } }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      yield* service.open("org-1");
      yield* Queue.fail(first, new Error("connection lost"));
      yield* Deferred.await(closedByDisconnect);
      assert.strictEqual(count, 1);
      const whileDown = yield* Effect.exit(service.open("org-2"));
      assert.strictEqual(whileDown._tag, "Failure");
      assert.strictEqual(subscriptions, 1);

      yield* TestClock.adjust("1 second");
      assert.strictEqual(subscriptions, 2);
      assert.strictEqual((yield* service.open("org-3")).status, "connected");
      yield* Queue.offer(second, JSON.stringify({ organizationId: "org-3", generation: "current" }));
      yield* Deferred.await(closedByRemoval);
      assert.strictEqual(count, 2);
    })).pipe(Effect.provide(runtime));
  }),
);

it.effect("a malformed notification does not stop the listener", () =>
  Effect.gen(function* () {
    const notifications = yield* Queue.make<string, Error>();
    const closed = yield* Deferred.make<void>();
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    }), Effect.succeed(Stream.fromQueue(notifications))).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => {
        Effect.runSync(Deferred.succeed(closed, undefined));
      } }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      yield* service.open("org-1");
      yield* Queue.offer(notifications, "not json");
      yield* Queue.offer(notifications, JSON.stringify({ organizationId: "org-1", generation: "current" }));
      yield* Deferred.await(closed);
      assert.strictEqual((yield* service.open("org-1")).status, "connected");
    })).pipe(Effect.provide(runtime));
  }),
);

it.effect("a delayed removal during loading does not cancel a replacement pairing", () =>
  Effect.gen(function* () {
    const loading = yield* Deferred.make<void>();
    const loaded = yield* Deferred.make<void>();
    let closed = 0;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.gen(function* () {
      yield* Deferred.succeed(loading, undefined);
      yield* Deferred.await(loaded);
      return { kind: "ready" as const, generation: "replacement", connections };
    })).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => { closed += 1; } }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      const opening = yield* service.open("org-1").pipe(Effect.forkChild);
      yield* Deferred.await(loading);
      yield* service.cancel("org-1", "old");
      yield* Deferred.succeed(loaded, undefined);
      assert.strictEqual((yield* Fiber.join(opening)).status, "connected");
      assert.strictEqual(closed, 0);
    })).pipe(Effect.provide(runtime));
    assert.strictEqual(closed, 1);
  }),
);

it.effect("does not load candidates before the notification subscription is ready", () =>
  Effect.gen(function* () {
    const subscribing = yield* Deferred.make<void>();
    const ready = yield* Deferred.make<void>();
    let loaded = false;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.sync(() => {
      loaded = true;
      return { kind: "missing" as const };
    }), Effect.gen(function* () {
      yield* Deferred.succeed(subscribing, undefined);
      yield* Deferred.await(ready);
      return Stream.never;
    })).pipe(Layer.provide(makePloyzLayer({ connect: async () => { throw new Error("must not dial"); } })));
    const opening = yield* Effect.scoped(Effect.gen(function* () {
      return yield* (yield* OrganizationRuntime).open("org-1");
    })).pipe(Effect.provide(runtime), Effect.forkChild);
    yield* Deferred.await(subscribing);
    assert.isFalse(loaded);
    yield* Deferred.succeed(ready, undefined);
    assert.deepStrictEqual(yield* Fiber.join(opening), { status: "no_connection" });
    assert.isTrue(loaded);
  }),
);

it.effect("subscription startup failure prevents candidate loading", () =>
  Effect.gen(function* () {
    let loaded = false;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.sync(() => {
      loaded = true;
      return { kind: "missing" as const };
    }), Effect.fail(new Error("LISTEN failed"))).pipe(
      Layer.provide(makePloyzLayer({ connect: async () => { throw new Error("must not dial"); } })),
    );
    const result = yield* Effect.exit(Effect.scoped(Effect.gen(function* () {
      return yield* (yield* OrganizationRuntime).open("org-1");
    })).pipe(Effect.provide(runtime)));
    assert.strictEqual(result._tag, "Failure");
    assert.isFalse(loaded);
  }),
);


it.effect("dials only the requested saved Machine and refuses an unknown Machine", () =>
  Effect.gen(function* () {
    const intended = "00000000000000000000000000000001" as MachineId;
    const unknown = "00000000000000000000000000000002" as MachineId;
    const candidate = { management: "ployz1:intended", machine_id: intended };
    const dialed: unknown[] = [];
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections: [...connections, candidate],
    })).pipe(Layer.provide(makePloyzLayer({
      connect: async (options) => {
        if (!("connections" in options)) throw new Error("expected shared connector");
        dialed.push(options.connections);
        return asTestDouble<Client>()({ close: async () => {} });
      },
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      assert.strictEqual((yield* service.open("org-1", intended)).status, "connected");
      assert.deepStrictEqual(yield* service.open("org-1", unknown), { status: "no_connection" });
      assert.deepStrictEqual(dialed, [[candidate]]);
    })).pipe(Effect.provide(runtime));
  }),
);

it.effect("bounds the connect phase and reports a hung handshake as unreachable", () =>
  Effect.gen(function* () {
    const dialing = yield* Deferred.make<void>();
    let aborted = false;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    })).pipe(Layer.provide(makePloyzLayer({
      connect: (options) => new Promise<Client>((_resolve, reject) => {
        options.signal?.addEventListener("abort", () => {
          aborted = true;
          reject(new Error("aborted"));
        }, { once: true });
        Effect.runSync(Deferred.succeed(dialing, undefined));
      }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      const opening = yield* service.open("org-1").pipe(Effect.forkChild);
      yield* Deferred.await(dialing);
      yield* TestClock.adjust(ORGANIZATION_CONNECT_TIMEOUT);
      const result = yield* Fiber.join(opening);
      assert.strictEqual(result.status, "unreachable");
      if (result.status === "unreachable") {
        assert.instanceOf(result.error, PloyzProviderError);
        assert.strictEqual(result.error?.operation, "connect");
      }
      assert.isTrue(aborted);
    })).pipe(Effect.provide(runtime));
  }),
);
