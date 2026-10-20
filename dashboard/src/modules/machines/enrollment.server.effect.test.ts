import type { Client, Connection, EnrollmentSnapshot } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Effect, Fiber, Layer, Queue } from "effect";
import * as TestClock from "effect/testing/TestClock";
import { asTestDouble } from "#/lib/test-double";
import { observeClusterEnrollment } from "#/modules/machines/enrollment.server";
import { makeOrganizationRuntimeLayer } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { noPairingChanges } from "#/test/organization-runtime";

const connections: Connection[] = [{ management: "ployz1:founder" }];
const snapshot = asTestDouble<EnrollmentSnapshot>()({ assignments: [] });

/** A Cluster whose connections query waits for the test to answer it, and whose snapshot comes once `observed` resolves. */
const slowQuery = Effect.gen(function* () {
  const queries = yield* Queue.unbounded<() => void>();
  let interrupted = 0;
  const layer = (observed: Promise<void>) => makeOrganizationRuntimeLayer(
    () => Effect.callback<void>((resume) => { Queue.offerUnsafe(queries, () => resume(Effect.void)); }).pipe(
      Effect.onInterrupt(() => Effect.sync(() => { interrupted += 1; })),
      Effect.as({ kind: "ready" as const, generation: "current", connections }),
    ),
    noPairingChanges,
  ).pipe(Layer.provide(makePloyzLayer({
    connect: async () => asTestDouble<Client>()({
      close: async () => {},
      observeEnrollment: async () => {
        await observed;
        return snapshot;
      },
    }),
  })));
  return { layer, queries, interrupted: () => interrupted };
});

it.effect("a join whose connections query takes 12 seconds lets the query finish and reads the Cluster's snapshot", () =>
  Effect.gen(function* () {
    const cluster = yield* slowQuery;
    const joining = yield* observeClusterEnrollment("org-1").pipe(
      Effect.map((joined) => joined.kind === "joined" ? joined.snapshot : joined.kind),
      Effect.scoped,
      Effect.provide(cluster.layer(Promise.resolve())),
      Effect.forkChild,
    );
    const query = yield* Queue.take(cluster.queries);
    yield* TestClock.adjust("12 seconds");
    query();
    assert.strictEqual(yield* Fiber.join(joining), snapshot);
    assert.strictEqual(cluster.interrupted(), 0);
  }),
);

it.effect("a Cluster that never answers a join reads unreachable after 10 seconds of Cluster time, however long the query took", () =>
  Effect.gen(function* () {
    const cluster = yield* slowQuery;
    const joining = yield* observeClusterEnrollment("org-1").pipe(
      Effect.map((joined) => joined.kind),
      Effect.scoped,
      Effect.provide(cluster.layer(new Promise<void>(() => {}))),
      Effect.forkChild,
    );
    const query = yield* Queue.take(cluster.queries);
    yield* TestClock.adjust("12 seconds");
    query();
    yield* Effect.promise(() => new Promise((resolve) => setTimeout(resolve, 0)));
    yield* TestClock.adjust("10 seconds");
    assert.strictEqual(yield* Fiber.join(joining), "unreachable");
    assert.strictEqual(cluster.interrupted(), 0);
  }),
);
