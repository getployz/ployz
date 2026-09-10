import { assert, it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import {
  executeScheduleNightlyBillingReconcile,
  executeSyncOrganizationBillingState,
  SYNC_ORGANIZATION_BILLING_STATE_SINGLETON,
  type BillingEffectRunner,
} from "#/modules/billing/inngest-sync/sync";
import { organization, organizationBillingState } from "#/db/schema";
import {
  Polar,
  type PolarService,
} from "#/modules/billing/polar-provider.server";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import type { PloyzStepTools } from "#/modules/inngest/client";
import {
  migrateTestDatabase,
  postgresTestContainer,
} from "#/test/postgres";
import { vi } from "vitest";

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

const polar = {
  mode: "hosted",
  productIds: {
    free: "00000000-0000-4000-8000-000000000101",
    solo: "00000000-0000-4000-8000-000000000102",
    teams: "00000000-0000-4000-8000-000000000103",
  },
  listActiveSubscriptions: () =>
    Effect.succeed([
      {
        id: "sub-teams",
        productId: "00000000-0000-4000-8000-000000000103",
        amount: 2900,
        currency: "usd",
        currentPeriodStart: new Date("2026-03-01T00:00:00.000Z"),
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
      },
    ]),
  createFreeSubscription: () => Effect.die("unused"),
  getProductPrices: () => Effect.die("unused"),
  updateSubscriptionPlan: () => Effect.die("unused"),
  createCheckout: () => Effect.die("unused"),
} satisfies PolarService;

it.live(
  "persists decoded Polar state and schedules reconciliation with stable Inngest ABI",
  () =>
    Effect.gen(function* () {
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                DATABASE_URL: container.url.href,
                APP_URL: "http://localhost:3000",
                BETTER_AUTH_SECRET: "better-auth-secret",
                GITHUB_CLIENT_ID: "github-client-id",
                GITHUB_CLIENT_SECRET: "github-client-secret",
                PLOYZ_RELAY_URL: "https://relay.example.test",
                APP_ENCRYPTION_SECRET:
                  "app-encryption-secret-at-least-32-characters",
              },
            }),
          ),
        ),
      );
      const layer = Layer.merge(
        DatabaseLive.pipe(Layer.provide(config)),
        Layer.succeed(Polar, polar),
      );
      const runEffect: BillingEffectRunner = (program) =>
        Effect.runPromise(program.pipe(Effect.provide(layer)));

      yield* Effect.promise(() =>
        runEffect(
          Effect.gen(function* () {
            const database = yield* Database;
            yield* database.drizzle.insert(organization).values({
              id: "00000000-0000-4000-8000-000000000001",
              name: "Acme",
              slug: "acme",
            });
          }),
        ),
      );

      const step = createStepTools();
      const result = yield* Effect.promise(() =>
        executeSyncOrganizationBillingState(
          {
            event: {
              data: {
                organizationId: "00000000-0000-4000-8000-000000000001",
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
        organizationId: "00000000-0000-4000-8000-000000000001",
        hasActiveSubscription: true,
        currentPlan: "teams",
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
        organizationId: "00000000-0000-4000-8000-000000000001",
        activeSubscriptionId: "sub-teams",
        currentPlan: "teams",
        productId: "00000000-0000-4000-8000-000000000103",
        amount: 2900,
        currency: "usd",
        currentPeriodStart: new Date("2026-03-01T00:00:00.000Z"),
        currentPeriodEnd: new Date("2026-04-01T00:00:00.000Z"),
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
                organizationId: "00000000-0000-4000-8000-000000000001",
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
