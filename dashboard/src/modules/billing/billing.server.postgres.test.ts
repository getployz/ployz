import { assert, it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { organizationBillingState } from "#/modules/billing/tables";
import type { CreatePolarCheckout, PolarService } from "#/modules/billing/polar-provider.server";
import { createEmbeddedCheckout, getBillingState, includesMonitoring } from "#/modules/billing/billing.server";
import { member, user } from "#/modules/identity/tables";
import { organizationMachine } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { holdProduction } from "#/test/billing";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedAnonymousStoreOrganization, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { createHash } from "node:crypto";

const ORGANIZATION = "00000000-0000-4000-8000-0000000000b1";
const ANONYMOUS = "00000000-0000-4000-8000-0000000000b2";
const SECRET = "ppair_fixture_store";
const slug = { organizationSlug: "shop" };

/** A hosted Polar that records the checkouts it was asked to open. */
const makePolar = () => {
  const checkouts: CreatePolarCheckout[] = [];
  const service: PolarService = {
    mode: "hosted",
    productId: "production",
    listActiveSubscriptions: () => Effect.succeed([]),
    createCheckout: (input) => Effect.sync(() => {
      checkouts.push(input);
      return { url: "https://polar.test/checkout" };
    }),
    setSubscriptionUnits: () => Effect.die("unused"),
    clearPendingUpdate: () => Effect.die("unused"),
    createCustomerPortal: () => Effect.die("unused"),
  };
  return { service, checkouts };
};

/** Joins Server `index` to the pairing `enrollStoreServer` made, as the Organization's `index`th Server. */
const addServer = Effect.fn(function* (index: number) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  yield* drizzle.insert(organizationMachine).values({
    organizationId: ORGANIZATION,
    machineId: machineId(String(index)),
    clusterKey: createHash("sha256").update(SECRET).digest("hex"),
    encryptedCapability: encryption.encrypt(`ployz1:cloud:${index}`),
    isDialEntry: false,
  });
});

it.live("opens checkout with one unit per Server, at least two", () =>
  Effect.gen(function* () {
    const polar = makePolar();
    const cloud = yield* storeTestCloud({ polar: polar.service });
    yield* Effect.gen(function* () {
      const userId = yield* seedStoreOrganization(ORGANIZATION);

      const none = yield* createEmbeddedCheckout({ userId }, slug);
      assert.deepStrictEqual(none, { url: "https://polar.test/checkout" });
      assert.strictEqual(polar.checkouts.at(-1)?.units, 2, "no Servers opens at the two units Polar requires");

      yield* enrollStoreServer(ORGANIZATION, { secret: SECRET, server: machineId("0") });
      yield* createEmbeddedCheckout({ userId }, slug);
      assert.strictEqual(polar.checkouts.at(-1)?.units, 2, "one Server opens at the two-unit minimum");

      for (const index of [1, 2, 3, 4]) yield* addServer(index);
      yield* createEmbeddedCheckout({ userId }, slug);
      assert.strictEqual(polar.checkouts.at(-1)?.units, 5, "five Servers open at five units");
      assert.strictEqual(polar.checkouts.at(-1)?.referenceId, ORGANIZATION);

      const state = yield* getBillingState({ userId }, slug);
      assert.strictEqual(state.servers, 5);
      assert.strictEqual(state.hasActiveSubscription, false);
    }).pipe(Effect.provide(cloud));
  }), 60_000);

it.live("refuses a second subscription for an Organization that holds one", () =>
  Effect.gen(function* () {
    const polar = makePolar();
    const cloud = yield* storeTestCloud({ polar: polar.service });
    yield* Effect.gen(function* () {
      const userId = yield* seedStoreOrganization(ORGANIZATION);
      const { drizzle } = yield* Database;
      yield* drizzle.insert(organizationBillingState).values({
        organizationId: ORGANIZATION,
        activeSubscriptionId: "sub-production",
        currentPeriodEnd: new Date("2027-01-01T00:00:00.000Z"),
        cancelAtPeriodEnd: false,
        hasActiveSubscription: true,
        syncedAt: new Date(),
      });

      const refused = yield* Effect.flip(createEmbeddedCheckout({ userId }, slug));

      assert.strictEqual(refused._tag, "Conflict");
      assert.deepStrictEqual(polar.checkouts, []);
    }).pipe(Effect.provide(cloud));
  }), 60_000);

it.live("opens no checkout for an unclaimed Organization or for someone outside it", () =>
  Effect.gen(function* () {
    const polar = makePolar();
    const cloud = yield* storeTestCloud({ polar: polar.service });
    yield* Effect.gen(function* () {
      yield* seedStoreOrganization(ORGANIZATION);
      const sandbox = yield* seedAnonymousStoreOrganization(ANONYMOUS, new Date(Date.now() + 86_400_000));

      const unclaimed = yield* Effect.flip(createEmbeddedCheckout({ userId: sandbox.userId }, { organizationSlug: `anon-${ANONYMOUS.slice(-8)}` }));
      assert.strictEqual(unclaimed._tag, "StoreRefused");

      const outsider = yield* Effect.flip(createEmbeddedCheckout({ userId: sandbox.userId }, slug));
      assert.notStrictEqual(outsider._tag, "StoreRefused");
      assert.deepStrictEqual(polar.checkouts, []);
    }).pipe(Effect.provide(cloud));
  }), 60_000);

it.live("leaves buying Production to owners", () =>
  Effect.gen(function* () {
    const polar = makePolar();
    const cloud = yield* storeTestCloud({ polar: polar.service });
    yield* Effect.gen(function* () {
      const ownerId = yield* seedStoreOrganization(ORGANIZATION);
      const { drizzle } = yield* Database;
      const [admin] = yield* drizzle.insert(user).values({ email: "grace@example.test", name: "Grace" }).returning();
      const adminId = admin?.id ?? "";
      yield* drizzle.insert(member).values({ userId: adminId, organizationId: ORGANIZATION, role: "admin" });

      assert.strictEqual((yield* getBillingState({ userId: ownerId }, slug)).canBuy, true);
      assert.strictEqual((yield* getBillingState({ userId: adminId }, slug)).canBuy, false, "an admin sees no Upgrade");
      const refused = yield* Effect.flip(createEmbeddedCheckout({ userId: adminId }, slug));
      assert.strictEqual(refused._tag, "Forbidden");
      assert.deepStrictEqual(polar.checkouts, []);
    }).pipe(Effect.provide(cloud));
  }), 60_000);

it.live("shows uptime and analytics to a Self-hosted Cloud's Organization whether or not its operator turns analytics on", () =>
  Effect.gen(function* () {
    for (const analytics of ["true", "false"]) {
      const cloud = yield* storeTestCloud({ env: { ANALYTICS_ENABLED: analytics } });
      yield* Effect.gen(function* () {
        yield* seedStoreOrganization(ORGANIZATION);
        assert.strictEqual(yield* includesMonitoring(ORGANIZATION), true, `ANALYTICS_ENABLED=${analytics} has no billing row to read`);
      }).pipe(Effect.provide(cloud));
    }
  }), 60_000);

it.live("shows uptime and analytics to a hosted Organization only while it holds Production", () =>
  Effect.gen(function* () {
    const cloud = yield* storeTestCloud({ polar: makePolar().service });
    yield* Effect.gen(function* () {
      yield* seedStoreOrganization(ORGANIZATION);
      assert.strictEqual(yield* includesMonitoring(ORGANIZATION), false, "no billing row is Free");

      yield* holdProduction(ORGANIZATION, false);
      assert.strictEqual(yield* includesMonitoring(ORGANIZATION), false, "a lapsed subscription is Free");

      const { drizzle } = yield* Database;
      yield* drizzle.update(organizationBillingState).set({
        hasActiveSubscription: true, activeSubscriptionId: "sub-production", currentPeriodEnd: new Date("2099-01-01T00:00:00.000Z"),
      }).where(eq(organizationBillingState.organizationId, ORGANIZATION));
      assert.strictEqual(yield* includesMonitoring(ORGANIZATION), true);
    }).pipe(Effect.provide(cloud));
  }), 60_000);
