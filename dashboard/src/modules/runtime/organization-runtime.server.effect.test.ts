import type { Client, Connection, MachineId } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Fiber, Layer } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import {
  makeOrganizationRuntimeLayer,
  ORGANIZATION_CONNECT_TIMEOUT,
  PAIRING_CHANGE_POLL,
  OrganizationRuntime,
} from "#/modules/runtime/organization-runtime.server";
import { OrganizationChangeLogFailure } from "#/modules/organization/change-log.server";
import { noPairingChanges } from "#/test/organization-runtime";
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
      noPairingChanges,
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
      noPairingChanges,
    ).pipe(Layer.provide(ployz));
    const unreachable = makeOrganizationRuntimeLayer(() =>
      Effect.succeed({ kind: "ready", generation: "grant-1", connections: [] }),
      noPairingChanges,
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
      noPairingChanges,
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
    }), noPairingChanges).pipe(Layer.provide(makePloyzLayer({
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
    }), noPairingChanges).pipe(Layer.provide(makePloyzLayer({
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
    }), noPairingChanges).pipe(Layer.provide(makePloyzLayer({
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

it.effect("a logged pairing change closes only sessions whose pairing was removed or replaced", () =>
  Effect.gen(function* () {
    const access = new Map<string, "current" | "replacement" | "missing">([["org-1", "current"], ["org-2", "current"], ["org-3", "current"]]);
    const changed = new Set<string>();
    const unreadable = new Set<string>();
    const closed: string[] = [];
    let dialing = "";
    let reads = 0;
    const runtime = makeOrganizationRuntimeLayer((organizationId) => Effect.sync(() => {
      const state = access.get(organizationId);
      return state === "missing" || state === undefined
        ? { kind: "missing" as const }
        : { kind: "ready" as const, generation: state, connections };
    }), {
      current: Effect.succeed("0"),
      changedSince: (organizationId, since) => {
        reads += 1;
        if (unreadable.has(organizationId)) return Effect.fail(new OrganizationChangeLogFailure({ cause: "log unavailable" }));
        const result = { cursor: `${Number(since) + 1}`, changed: changed.has(organizationId) };
        changed.delete(organizationId);
        return Effect.succeed(result);
      },
    }).pipe(Layer.provide(makePloyzLayer({
      connect: async () => {
        const organizationId = dialing;
        return asTestDouble<Client>()({ close: async () => { closed.push(organizationId); } });
      },
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      for (const organizationId of ["org-1", "org-2", "org-3"]) {
        dialing = organizationId;
        assert.strictEqual((yield* service.open(organizationId)).status, "connected");
      }
      // An unrelated pairing write keeps the session; unlogged removals wait for the log.
      changed.add("org-1");
      access.set("org-2", "missing");
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      assert.deepStrictEqual(closed, []);

      changed.add("org-2");
      access.set("org-1", "replacement");
      changed.add("org-1");
      unreadable.add("org-3");
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      assert.deepStrictEqual(closed.sort(), ["org-1", "org-2", "org-3"]);
      // Closed sessions stop reading the log.
      const readsAtClose = reads;
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      assert.strictEqual(reads, readsAtClose);
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
    }), noPairingChanges).pipe(Layer.provide(makePloyzLayer({
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

it.effect("dials only the requested saved Machine and refuses an unknown Machine", () =>
  Effect.gen(function* () {
    const intended = "00000000000000000000000000000001" as MachineId;
    const unknown = "00000000000000000000000000000002" as MachineId;
    const candidate = { management: "ployz1:intended", machine_id: intended };
    const dialed: unknown[] = [];
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections: [...connections, candidate],
    }), noPairingChanges).pipe(Layer.provide(makePloyzLayer({
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
    }), noPairingChanges).pipe(Layer.provide(makePloyzLayer({
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

it.effect("lets an in-flight pairing check finish when the caller's scope closes, so no query is cancelled", () =>
  Effect.gen(function* () {
    const started = yield* Deferred.make<void>();
    const release = yield* Deferred.make<void>();
    let interrupted = false;
    let finished = false;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    }), {
      current: Effect.succeed("0"),
      changedSince: () => Effect.gen(function* () {
        yield* Deferred.succeed(started, undefined);
        yield* Deferred.await(release);
        finished = true;
        return { cursor: "1", changed: false };
      }).pipe(Effect.onInterrupt(() => Effect.sync(() => { interrupted = true; }))),
    }).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => {} }),
    })));
    yield* Effect.gen(function* () {
      const service = yield* OrganizationRuntime;
      const session = yield* Effect.scoped(Effect.gen(function* () {
        assert.strictEqual((yield* service.open("org-1")).status, "connected");
        yield* Deferred.await(started);
      })).pipe(Effect.forkChild);
      yield* Deferred.await(started);
      // Let the caller's scope start closing while the check is still waiting.
      yield* Effect.repeat(Effect.yieldNow, { times: 20 });
      assert.isFalse(interrupted);
      yield* Deferred.succeed(release, undefined);
      yield* Fiber.join(session);
      assert.isTrue(finished);
      assert.isFalse(interrupted);
    }).pipe(Effect.provide(runtime));
  }),
);
