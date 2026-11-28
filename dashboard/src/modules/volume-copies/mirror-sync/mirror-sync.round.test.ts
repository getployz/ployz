import { it } from "@effect/vitest";
import { Effect, Fiber } from "effect";
import { TestClock } from "effect/testing";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { refreshMirrors } from "#/modules/volume-copies/mirror-sync/mirror-sync.round.server";
import { clusterShowing } from "#/modules/volume-run/volume-run.test-fixture";

it.effect("a Cluster that never says which copies it holds refreshes nothing once its 5 seconds are spent", () =>
  Effect.gen(function* () {
    const refreshing = yield* refreshMirrors("org", null, () => Effect.never).pipe(
      Effect.provideService(OrganizationRuntime, clusterShowing(["web-1"])),
      // Neither the Store nor the database is read before the copies come back.
      Effect.provideService(CloudStore, asTestDouble<CloudStore["Service"]>()({})),
      Effect.provideService(Database, asTestDouble<Database["Service"]>()({})),
      Effect.provideService(InngestClient, asTestDouble<InngestClient["Service"]>()({})),
      Effect.forkChild,
    );
    yield* TestClock.adjust("5 seconds");
    expect(yield* Fiber.join(refreshing)).toEqual([]);
  }), 2_000);
