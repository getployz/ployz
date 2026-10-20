import type { Client, Connection, MachineId, RuntimeWatchView } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Deferred, Effect, Fiber, Layer } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import {
  ClusterOutOfTime,
  clusterRead,
  firstRuntimeFrame,
  makeOrganizationRuntimeLayer,
  operationRuntimeFrame,
  ORGANIZATION_CONNECT_TIMEOUT,
  PAIRING_CHANGE_POLL,
  PAIRING_CHECK_FAILURE_LIMIT,
  OrganizationRuntime,
  withClusterBudget,
} from "#/modules/runtime/organization-runtime.server";
import { OrganizationChangeLogFailure } from "#/modules/organization/change-log.server";
import { noPairingChanges } from "#/test/organization-runtime";
import { makePloyzLayer, PloyzProviderError } from "#/modules/runtime/ployz.server";

const connections: Connection[] = [
  { management: "ployz1:preferred" },
  { management: "ployz1:spare" },
];

/** A Runtime Watch frame reporting one Machine, `machine-1`, named `name`. */
const named = (name: string) => asTestDouble<RuntimeWatchView>()({ machines: [{ machine: { id: "machine-1", name } }] });

/** A runtime whose one Cluster streams `frames`, recording each with `recordFrame`. */
const streamingRuntime = (
  frames: readonly RuntimeWatchView[],
  recordFrame: Parameters<typeof makeOrganizationRuntimeLayer>[2],
) => makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "current", connections }), noPairingChanges, recordFrame)
  .pipe(Layer.provide(makePloyzLayer({
    connect: async () => asTestDouble<Client>()({
      close: async () => {},
      runtime: { watch: async function* () { yield* frames; } },
    }),
  })));

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
    const access = new Map<string, "current" | "replacement" | "missing">([["org-1", "current"], ["org-2", "current"]]);
    const changed = new Set<string>();
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
      for (const organizationId of ["org-1", "org-2"]) {
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
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      assert.deepStrictEqual(closed.sort(), ["org-1", "org-2"]);
      // Closed sessions stop reading the log.
      const readsAtClose = reads;
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      assert.strictEqual(reads, readsAtClose);
    })).pipe(Effect.provide(runtime));
  }),
);

it.effect("an unreadable change log closes the session only after consecutive failed checks", () =>
  Effect.gen(function* () {
    let unreadable = false;
    let closed = 0;
    const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({
      kind: "ready", generation: "current", connections,
    }), {
      current: Effect.succeed("0"),
      changedSince: (_organizationId, since) => unreadable
        ? Effect.fail(new OrganizationChangeLogFailure({ cause: "log unavailable" }))
        : Effect.succeed({ cursor: `${Number(since) + 1}`, changed: false }),
    }).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => { closed += 1; } }),
    })));
    const polls = (count: number) => Effect.repeat(TestClock.adjust(PAIRING_CHANGE_POLL), { times: count - 1 });
    yield* Effect.scoped(Effect.gen(function* () {
      assert.strictEqual((yield* (yield* OrganizationRuntime).open("org-1")).status, "connected");
      yield* polls(1);
      unreadable = true;
      yield* polls(PAIRING_CHECK_FAILURE_LIMIT - 1);
      assert.strictEqual(closed, 0, "a failure run shorter than the limit keeps the session");
      unreadable = false;
      yield* polls(1);
      unreadable = true;
      yield* polls(PAIRING_CHECK_FAILURE_LIMIT - 1);
      assert.strictEqual(closed, 0, "a readable check resets the failure run");
      yield* polls(1);
      assert.strictEqual(closed, 1, "the limit of consecutive failures closes the session");
    })).pipe(Effect.provide(runtime));
  }),
);

it.effect("a removal whose pairing load failed is still seen by the next check", () =>
  Effect.gen(function* () {
    let pairing: "current" | "unloadable" | "missing" = "current";
    let logged = false;
    let closed = 0;
    const runtime = makeOrganizationRuntimeLayer(() => {
      if (pairing === "unloadable") return Effect.fail(new Error("database unavailable"));
      return Effect.succeed(pairing === "missing" ? { kind: "missing" as const } : { kind: "ready" as const, generation: "current", connections });
    }, {
      current: Effect.succeed("0"),
      changedSince: (_organizationId, since) => Effect.succeed(logged
        ? { cursor: "removal", changed: since !== "removal" }
        : { cursor: since, changed: false }),
    }).pipe(Layer.provide(makePloyzLayer({
      connect: async () => asTestDouble<Client>()({ close: async () => { closed += 1; } }),
    })));
    yield* Effect.scoped(Effect.gen(function* () {
      assert.strictEqual((yield* (yield* OrganizationRuntime).open("org-1")).status, "connected");
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      logged = true;
      pairing = "unloadable";
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      assert.strictEqual(closed, 0);
      pairing = "missing";
      yield* TestClock.adjust(PAIRING_CHANGE_POLL);
      assert.strictEqual(closed, 1);
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

it.effect("records a frame read once after the read and its deadlines, so a slow write finishes and is never interrupted", () =>
  Effect.gen(function* () {
    const reads = {
      // Inside firstRuntimeFrame's 5 second timeout, and a 1 second Cluster budget.
      firstRuntimeFrame: firstRuntimeFrame("org-1"),
      operationRuntimeFrame: operationRuntimeFrame("org-1").pipe(withClusterBudget(1_000)),
    };
    for (const [reader, frameRead] of Object.entries(reads)) {
      const writing = yield* Deferred.make<void>();
      let written = false;
      let interrupted = false;
      const runtime = streamingRuntime([named("web-1")], () => Effect.gen(function* () {
        yield* Deferred.succeed(writing, undefined);
        // Longer than every deadline on the read.
        yield* Effect.sleep("6 seconds");
        written = true;
      }).pipe(Effect.onInterrupt(() => Effect.sync(() => { interrupted = true; }))));
      yield* Effect.gen(function* () {
        const read = yield* frameRead.pipe(Effect.forkChild);
        yield* Deferred.await(writing);
        yield* TestClock.adjust("6 seconds");
        assert.deepStrictEqual(yield* Fiber.join(read), named("web-1"), reader);
        assert.isTrue(written, reader);
        assert.isFalse(interrupted, reader);
      }).pipe(Effect.provide(runtime));
    }
  }),
);

it.effect("retries a failed name write on the next frame until it lands", () =>
  Effect.gen(function* () {
    let stored: string | undefined;
    let failuresLeft = 2;
    let writes = 0;
    const frames = [named("one"), named("two"), named("two"), named("two"), named("two")];
    const runtime = streamingRuntime(frames, (_organizationId, frame) => Effect.suspend(() => {
      writes += 1;
      const name = frame.machines[0]?.machine.name;
      if (name === "two" && failuresLeft > 0) {
        failuresLeft -= 1;
        return Effect.fail(new Error("database unavailable"));
      }
      stored = name;
      return Effect.void;
    }));
    const read = yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* (yield* OrganizationRuntime).open("org-1");
      if (session.status !== "connected") throw new Error("Cluster not connected");
      const watched = yield* session.connected.watch();
      return yield* Effect.promise(async () => {
        const frames: RuntimeWatchView[] = [];
        for await (const frame of watched) frames.push(frame);
        return frames;
      });
    })).pipe(Effect.provide(runtime));
    // A failed write never fails the read: every frame still arrives.
    assert.strictEqual(read.length, frames.length);
    assert.strictEqual(stored, "two");
    // Once `two` lands, unchanged frames write nothing.
    assert.strictEqual(writes, 4);
  }),
);

it.effect("Cluster reads in flight together spend their budget once, and time between reads spends none of it", () =>
  Effect.gen(function* () {
    const reading = Effect.gen(function* () {
      yield* Effect.all([clusterRead(Effect.sleep("6 seconds")), clusterRead(Effect.sleep("6 seconds"))], { concurrency: "unbounded" });
      yield* Effect.sleep("100 seconds");
      return yield* clusterRead(Effect.never);
    }).pipe(withClusterBudget(10_000), Effect.flip, Effect.forkChild);
    const read = yield* reading;
    yield* TestClock.adjust("106 seconds");
    yield* Effect.promise(() => new Promise((resolve) => setTimeout(resolve, 0)));
    assert.isUndefined(read.pollUnsafe());
    yield* TestClock.adjust("4 seconds");
    assert.isTrue((yield* Fiber.join(read)) instanceof ClusterOutOfTime);
  }),
);
