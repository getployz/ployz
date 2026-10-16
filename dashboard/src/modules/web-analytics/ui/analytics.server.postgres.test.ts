import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { addDays, localDay } from "#/modules/web-analytics/local-time";
import { dayOf, ensurePartitions, forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { analyticsGoal, analyticsRequest } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { createGoal, deleteGoal, readAnalyticsView } from "./analytics.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f501";
const PROJECT = "00000000-0000-4000-8000-00000000f502";
const PRODUCTION = "00000000-0000-4000-8000-00000000f503";
const KEPT_GOAL = "00000000-0000-4000-8000-00000000f504";
const scope = { organizationSlug: "acme", environmentId: PRODUCTION };

const hosted: PolarService = {
  mode: "hosted",
  productId: "pro",
  listActiveSubscriptions: () => Effect.die("not used"),
  createCustomerPortal: () => Effect.die("not used"),
};

/** Acme's production Environment with one goal on `/kept`, on the given plan, with analytics on or off there. */
const setup = Effect.fn(function* (plan: { polar?: PolarService; analyticsEnabled: boolean }, siteEnabled: boolean) {
  forgetPartitions();
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

it.live("every hosted Cloud Organization writes goals, with no subscription and ANALYTICS_ENABLED unset", () => Effect.gen(function* () {
  const { provide, goals, actor } = yield* setup({ polar: hosted, analyticsEnabled: false }, true);
  yield* provide(createGoal(actor, { ...scope, name: "Signup", pathPattern: "/signup*" }));
  expect(yield* goals()).toEqual(["/kept", "/signup*"]);
}).pipe(Effect.scoped));

const insertPageviews = (rows: ReadonlyArray<{ ts: Date; visitorId: bigint | null }>) => Effect.gen(function* () {
  yield* ensurePartitions(rows.map((row) => dayOf(row.ts)));
  const { drizzle } = yield* Database;
  yield* drizzle.insert(analyticsRequest).values(rows.map((row, index) => ({
    ...row, environmentId: PRODUCTION, requestId: `r${index}`, service: "web", host: "shop.test", method: "GET", path: "/",
    status: 200, durationMs: 1, bytesIn: 0, bytesOut: 0, kind: "pageview" as const, source: "direct",
  })));
}).pipe(Effect.orDie);

it.live("a site whose every visit was lost says which days, not that it has no visits, and keeps its goals", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ analyticsEnabled: true }, true);
  const today = localDay("UTC", Date.now());
  const day = (offset: number) => addDays("UTC", today, offset);
  yield* provide(insertPageviews([day(-6), day(-6), day(-4)].map((at) => ({ ts: new Date(`${at}T12:00:00Z`), visitorId: null }))));
  const view = yield* provide(readAnalyticsView(actor, { ...scope, range: "30d" }));
  expect(view).toMatchObject({ gate: "on", empty: true, lostDays: [day(-6), day(-4)], goals: [{ pathPattern: "/kept", visitors: 0 }] });
}).pipe(Effect.scoped));

it.live("a day with pageviews no salt could count is partial, and so is today", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ analyticsEnabled: true }, true);
  const today = localDay("UTC", Date.now());
  const day = (offset: number) => addDays("UTC", today, offset);
  yield* provide(insertPageviews([
    { ts: new Date(`${day(-9)}T12:00:00Z`), visitorId: 1n },
    { ts: new Date(`${day(-5)}T12:00:00Z`), visitorId: 1n },
    { ts: new Date(`${day(-5)}T13:00:00Z`), visitorId: null },
  ]));
  const view = yield* provide(readAnalyticsView(actor, { ...scope, range: "30d" }));
  if (view.gate !== "on" || view.empty) return expect.unreachable("the site has a pageview");
  expect(view.partialDays).toEqual([{ day: day(-5), reason: "no-salt" }, { day: today, reason: "today" }]);
}).pipe(Effect.scoped));
