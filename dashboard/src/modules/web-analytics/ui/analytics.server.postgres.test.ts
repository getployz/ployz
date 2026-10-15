import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { addDays, localDay } from "#/modules/web-analytics/local-time";
import { ensurePartitions, forgetPartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import { analyticsGapDaily, analyticsGoal, analyticsPageview, analyticsRollupState } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { createGoal, deleteGoal, readAnalyticsView } from "./analytics.server";

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

it.live("a site whose every visit was lost says which days, not that it has no visits, and keeps its goals", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ analyticsEnabled: true }, true);
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.insert(analyticsGapDaily).values([
      { environmentId: PRODUCTION, day: "2026-10-11", count: 0 },
      { environmentId: PRODUCTION, day: "2026-10-12", count: 40 },
      { environmentId: PRODUCTION, day: "2026-10-10", count: 12 },
    ]);
  }).pipe(Effect.orDie));
  const view = yield* provide(readAnalyticsView(actor, { ...scope, range: "30d" }));
  expect(view).toMatchObject({ gate: "on", empty: true, lostDays: ["2026-10-10", "2026-10-12"], goals: [{ pathPattern: "/kept", visitors: 0 }] });
}).pipe(Effect.scoped));

it.live("a past day is partial until the rollup closes it, and again while late pageviews dirty it", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ analyticsEnabled: true }, true);
  const today = localDay("UTC", Date.now());
  const day = (offset: number) => addDays("UTC", today, offset);
  const first = new Date(`${day(-20)}T12:00:00Z`);
  const read = (rollup: { rolledUpTo: string | null; dirty: string[] }) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.update(analyticsRollupState).set(rollup).where(eq(analyticsRollupState.environmentId, PRODUCTION));
    const view = yield* readAnalyticsView(actor, { ...scope, range: "30d" });
    if (view.gate !== "on" || view.empty) return expect.unreachable("the site has a pageview");
    return view.partialDays.filter((partial) => partial.reason === "late").map((partial) => partial.day);
  }));
  yield* provide(Effect.gen(function* () {
    yield* ensurePartitions([monthOf(first)]);
    const { drizzle } = yield* Database;
    yield* drizzle.insert(analyticsPageview).values({ environmentId: PRODUCTION, ts: first, visitorId: 1n, host: "shop.test", pathname: "/", source: "direct" });
  }).pipe(Effect.orDie));

  expect(yield* read({ rolledUpTo: day(-3), dirty: [] })).toEqual([day(-2), day(-1)]);
  expect(yield* read({ rolledUpTo: day(-1), dirty: [day(-9)] })).toEqual([day(-9)]);
  expect(yield* read({ rolledUpTo: day(-1), dirty: [] })).toEqual([]);
  expect((yield* read({ rolledUpTo: null, dirty: [] }))[0]).toEqual(day(-20));
}).pipe(Effect.scoped));
