import { createHash } from "node:crypto";
import { testConfigEnvironment } from "#/test/config-environment";
import { assert, it } from "@effect/vitest";
import { and, eq } from "drizzle-orm";
import { ConfigProvider, Effect, Layer } from "effect";
import {
  executeScheduleNightlyBillingReconcile,
  executeSyncOrganizationBillingState,
  SYNC_ORGANIZATION_BILLING_STATE_SINGLETON,
  type BillingEffectRunner,
} from "#/modules/billing/inngest-sync/sync";
import type { PolarSubscription } from "#/modules/billing/billing";
import { organization, organizationBillingState } from "#/db/schema";
import {
  Polar,
  type PolarService,
} from "#/modules/billing/polar-provider.server";
import { organizationMachine } from "#/modules/machines/tables";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { machineId } from "#/test/ids";
import {
  postgresTestDatabase,
} from "#/test/postgres";
import { enrollStoreServer } from "#/test/store-cloud";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { vi } from "vitest";

const ORGANIZATION = "00000000-0000-4000-8000-000000000001";
const PRODUCT = "00000000-0000-4000-8000-000000000102";
const SECRET = "ppair_fixture_store";
const encryption = makeSecretEncryption("billing-sync-test-encryption-secret");

function createStepTools() {
  const run: PloyzStepTools["run"] = async (_id, operation, ...input) => {
    const result = await operation(...input);
    return result === undefined ? null : JSON.parse(JSON.stringify(result));
  };
  return {
    run,
    sendEvent: vi.fn(async () => ({ ids: [] })),
  };
}

type HeldSubscription = { subscription: PolarSubscription };

/** A hosted Polar holding one subscription. Setting units behaves as Polar does: an invoice applies at once, a next-period change goes pending. */
function makePolar(initial: Partial<PolarSubscription> = {}) {
  const current: HeldSubscription = {
    subscription: {
      id: "sub-production",
      productId: PRODUCT,
      currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
      cancelAtPeriodEnd: true,
      units: 2,
      pendingUpdate: null,
      ...initial,
    },
  };
  const calls: unknown[] = [];
  const service = {
    mode: "hosted",
    productId: PRODUCT,
    listActiveSubscriptions: () => Effect.succeed([current.subscription]),
    createCheckout: () => Effect.die("unused"),
    setSubscriptionUnits: (input) =>
      Effect.sync(() => {
        calls.push(input);
        current.subscription = input.prorationBehavior === "invoice"
          ? { ...current.subscription, units: input.units, pendingUpdate: null }
          : { ...current.subscription, pendingUpdate: { units: input.units } };
      }),
    clearPendingUpdate: (subscriptionId) =>
      Effect.sync(() => {
        calls.push({ clearPendingUpdate: subscriptionId });
        current.subscription = { ...current.subscription, pendingUpdate: null };
      }),
    createCustomerPortal: () => Effect.die("unused"),
  } satisfies PolarService;
  return { service, calls, current };
}

const harness = Effect.fn(function* (polar: PolarService) {
  const testDatabase = yield* postgresTestDatabase;
  const config = AppConfig.layer.pipe(
    Layer.provide(
      ConfigProvider.layer(
        ConfigProvider.fromEnv({
          env: {
            ...testConfigEnvironment(),
            DATABASE_URL: testDatabase.url.href,
          },
        }),
      ),
    ),
  );
  const layer = Layer.mergeAll(
    DatabaseLive.pipe(Layer.provide(config)),
    Layer.succeed(Polar, polar),
    Layer.succeed(SecretEncryption, encryption),
  );
  const runEffect: BillingEffectRunner = (program) =>
    Effect.runPromise(program.pipe(Effect.provide(layer)));
  yield* Effect.promise(() =>
    runEffect(
      Effect.gen(function* () {
        const database = yield* Database;
        yield* database.drizzle.insert(organization).values({
          id: ORGANIZATION,
          name: "Acme",
          slug: "acme",
        });
      }),
    ),
  );
  const syncOnce = () =>
    Effect.promise(() =>
      executeSyncOrganizationBillingState(
        { event: { data: { organizationId: ORGANIZATION, reason: "subscription.updated" } }, step: createStepTools() },
        runEffect,
      ),
    );
  const addServer = (index: number) =>
    Effect.promise(() =>
      runEffect(
        Effect.gen(function* () {
          const { drizzle } = yield* Database;
          yield* drizzle.insert(organizationMachine).values({
            organizationId: ORGANIZATION,
            machineId: machineId(String(index)),
            clusterKey: createHash("sha256").update(SECRET).digest("hex"),
            encryptedCapability: encryption.encrypt(`ployz1:cloud:${index}`),
            isDialEntry: false,
          });
        }),
      ),
    );
  const removeServer = (index: number) =>
    Effect.promise(() =>
      runEffect(
        Effect.gen(function* () {
          const { drizzle } = yield* Database;
          yield* drizzle.delete(organizationMachine).where(and(
            eq(organizationMachine.organizationId, ORGANIZATION),
            eq(organizationMachine.machineId, machineId(String(index))),
          ));
        }),
      ),
    );
  /** Pairs the Organization with its first Server and joins the others to that pairing. */
  const enrollServers = Effect.fn(function* (servers: number) {
    yield* Effect.promise(() =>
      runEffect(enrollStoreServer(ORGANIZATION, { secret: SECRET, server: machineId("0") })),
    );
    for (let index = 1; index < servers; index += 1) yield* addServer(index);
  });
  return { runEffect, syncOnce, enrollServers, addServer, removeServer };
});

it.live(
  "persists decoded Polar state and schedules reconciliation with stable Inngest ABI",
  () =>
    Effect.gen(function* () {
      const { service } = makePolar();
      const { runEffect } = yield* harness(service);

      const step = createStepTools();
      const result = yield* Effect.promise(() =>
        executeSyncOrganizationBillingState(
          {
            event: {
              data: {
                organizationId: ORGANIZATION,
                reason: "subscription.updated",
                sourceUpdatedAt: "2026-03-27T00:00:00.000Z",
              },
            },
            step,
          },
          runEffect,
        ),
      );

      assert.deepStrictEqual(result, {
        organizationId: ORGANIZATION,
        hasActiveSubscription: true,
      });
      const stored = yield* Effect.promise(() =>
        runEffect(
          Effect.gen(function* () {
            const database = yield* Database;
            return yield* database.drizzle
              .select()
              .from(organizationBillingState);
          }),
        ),
      );
      assert.strictEqual(stored.length, 1);
      const storedSnapshot = stored[0];
      if (storedSnapshot === undefined) assert.fail("Missing billing snapshot");
      assert.deepStrictEqual(storedSnapshot, {
        organizationId: ORGANIZATION,
        activeSubscriptionId: "sub-production",
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
        cancelAtPeriodEnd: true,
        hasActiveSubscription: true,
        syncedAt: storedSnapshot.syncedAt,
        sourceUpdatedAt: new Date("2026-03-27T00:00:00.000Z"),
      });

      const scheduleStep = createStepTools();
      const scheduled = yield* Effect.promise(() =>
        executeScheduleNightlyBillingReconcile(
          { step: scheduleStep },
          runEffect,
        ),
      );
      assert.deepStrictEqual(scheduled, { organizationCount: 1 });
      assert.deepStrictEqual(scheduleStep.sendEvent.mock.calls, [
        [
          "request-billing-syncs",
          [
            {
              name: "billing/organization-sync.requested",
              data: {
                organizationId: ORGANIZATION,
                reason: "nightly-reconcile",
              },
            },
          ],
        ],
      ]);
      assert.deepStrictEqual(SYNC_ORGANIZATION_BILLING_STATE_SINGLETON, {
        key: "event.data.organizationId",
        mode: "cancel",
      });
    }),
  60_000,
);

it.live(
  "keeps the subscription's units at the Server count, and says nothing when they already match",
  () =>
    Effect.gen(function* () {
      const polar = makePolar({ units: 2 });
      const { syncOnce, enrollServers } = yield* harness(polar.service);

      yield* syncOnce();
      assert.deepStrictEqual(polar.calls, [], "two Servers or fewer stay at the two units the plan includes");

      yield* enrollServers(4);
      yield* syncOnce();
      assert.deepStrictEqual(polar.calls, [{ subscriptionId: "sub-production", units: 4, prorationBehavior: "invoice" }]);
      assert.strictEqual(polar.current.subscription.units, 4);

      yield* syncOnce();
      assert.strictEqual(polar.calls.length, 1, "a second sync at the same Server count sends nothing");
    }),
  60_000,
);

it.live(
  "schedules a decrease for the next period once, and raises the units while one is pending",
  () =>
    Effect.gen(function* () {
      const polar = makePolar({ units: 4 });
      const { syncOnce, enrollServers, addServer, removeServer } = yield* harness(polar.service);

      yield* enrollServers(4);
      yield* syncOnce();
      assert.deepStrictEqual(polar.calls, []);

      yield* removeServer(3);
      yield* syncOnce();
      assert.deepStrictEqual(polar.calls, [{ subscriptionId: "sub-production", units: 3, prorationBehavior: "next_period" }]);
      assert.deepStrictEqual(polar.current.subscription.pendingUpdate, { units: 3 });

      yield* syncOnce();
      assert.strictEqual(polar.calls.length, 1, "the scheduled decrease already matches, so a second sync sends nothing");

      yield* addServer(3);
      yield* syncOnce();
      assert.deepStrictEqual(polar.calls.at(-1), { clearPendingUpdate: "sub-production" }, "the Server came back, so the scheduled decrease goes");
    }),
  60_000,
);

it.live(
  "bills no subscription when the Organization holds none",
  () =>
    Effect.gen(function* () {
      const polar = makePolar();
      const none: PolarService = { ...polar.service, listActiveSubscriptions: () => Effect.succeed([]) };
      const { syncOnce, enrollServers } = yield* harness(none);

      yield* enrollServers(5);
      const result = yield* syncOnce();

      assert.deepStrictEqual(result, { organizationId: ORGANIZATION, hasActiveSubscription: false });
      assert.deepStrictEqual(polar.calls, []);
    }),
  60_000,
);
