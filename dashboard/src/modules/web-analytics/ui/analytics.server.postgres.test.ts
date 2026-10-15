import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { analyticsGoal } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { createGoal, deleteGoal } from "./analytics.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f501";
const PROJECT = "00000000-0000-4000-8000-00000000f502";
const PRODUCTION = "00000000-0000-4000-8000-00000000f503";
const KEPT_GOAL = "00000000-0000-4000-8000-00000000f504";
const scope = { organizationSlug: "acme", environmentId: PRODUCTION };

const unpaid: PolarService = {
  mode: "hosted",
  productId: "pro",
  listActiveSubscriptions: () => Effect.die("not used"),
  createCustomerPortal: () => Effect.die("not used"),
};

/** Acme's production Environment with one goal on `/kept`, on the given plan, with analytics on or off there. */
const setup = Effect.fn(function* (plan: { polar?: PolarService; analyticsEnabled: boolean }, siteEnabled: boolean) {
  const services = yield* Layer.build(yield* storeTestCloud({
    ...(plan.polar && { polar: plan.polar }),
    env: { ANALYTICS_ENABLED: String(plan.analyticsEnabled) },
  }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  const store = yield* provide(cloudStore);
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: PRODUCTION }));
  yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: siteEnabled, timezone: "UTC" }));
  const goals = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return (yield* drizzle.select({ pathPattern: analyticsGoal.pathPattern }).from(analyticsGoal)
      .where(eq(analyticsGoal.environmentId, PRODUCTION))).map((goal) => goal.pathPattern).sort();
  }).pipe(Effect.orDie));
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.insert(analyticsGoal).values({ id: KEPT_GOAL, environmentId: PRODUCTION, name: "Kept", pathPattern: "/kept" });
  }).pipe(Effect.orDie));
  return { provide, goals, actor: { userId } };
});

const refusals = [
  { name: "an unpaid Cloud plan", plan: { polar: unpaid, analyticsEnabled: true }, siteEnabled: true, refusal: "Forbidden" },
  { name: "a self-hosted Cloud with analytics off", plan: { analyticsEnabled: false }, siteEnabled: true, refusal: "Forbidden" },
  { name: "an Environment with analytics off", plan: { analyticsEnabled: true }, siteEnabled: false, refusal: "NotFound" },
] as const;

for (const { name, plan, siteEnabled, refusal } of refusals) {
  it.live(`goal writes are refused on ${name}, and leave the goals as they were`, () => Effect.gen(function* () {
    const { provide, goals, actor } = yield* setup(plan, siteEnabled);
    const created = yield* provide(Effect.flip(createGoal(actor, { ...scope, name: "Signup", pathPattern: "/signup*" })));
    const deleted = yield* provide(Effect.flip(deleteGoal(actor, { ...scope, id: KEPT_GOAL })));
    expect([created._tag, deleted._tag]).toEqual([refusal, refusal]);
    expect(yield* goals()).toEqual(["/kept"]);
  }).pipe(Effect.scoped));
}

it.live("goal writes go through where the plan and the Environment have analytics on", () => Effect.gen(function* () {
  const { provide, goals, actor } = yield* setup({ analyticsEnabled: true }, true);
  yield* provide(createGoal(actor, { ...scope, name: "Signup", pathPattern: "/signup*" }));
  yield* provide(deleteGoal(actor, { ...scope, id: KEPT_GOAL }));
  expect(yield* goals()).toEqual(["/signup*"]);
}).pipe(Effect.scoped));
