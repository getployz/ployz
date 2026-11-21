import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { expect } from "vitest";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { analyticsGoal } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { holdProduction, hostedPolar } from "#/test/billing";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { createGoal, deleteGoal, readAnalyticsView, readApiView, readErrorsView } from "./analytics.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f501";
const PROJECT = "00000000-0000-4000-8000-00000000f502";
const PRODUCTION = "00000000-0000-4000-8000-00000000f503";
const KEPT_GOAL = "00000000-0000-4000-8000-00000000f504";
const scope = { organizationSlug: "acme", environmentId: PRODUCTION };

/** Acme's production Environment with one goal on `/kept`, on the given plan, with analytics on or off there. A hosted Cloud reads Production from the billing row, so `production` seeds it. */
const setup = Effect.fn(function* (plan: { polar?: PolarService; analyticsEnabled: boolean; clickhouse?: boolean }, siteEnabled: boolean, production = false) {
  const services = yield* storeTestCloud({
    ...(plan.polar && { polar: plan.polar }),
    // The gate only asks whether ClickHouse is configured; a view over a site with no Services reads none of it.
    env: { ANALYTICS_ENABLED: String(plan.analyticsEnabled), ...(plan.clickhouse !== false && { OBSERVE_CLICKHOUSE_URL: "http://127.0.0.1:1" }) },
  });
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  if (production) yield* provide(holdProduction(ORGANIZATION));
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
  const { provide, goals, actor } = yield* setup({ polar: hostedPolar, analyticsEnabled: false }, true);
  yield* provide(createGoal(actor, { ...scope, name: "Signup", pathPattern: "/signup*" }));
  expect(yield* goals()).toEqual(["/kept", "/signup*"]);
}).pipe(Effect.scoped));

for (const { name, plan, siteEnabled } of refusals) {
  it.live(`the API and Errors views read nothing on ${name}`, () => Effect.gen(function* () {
    const { provide, actor } = yield* setup(plan, siteEnabled);
    const closed = plan.analyticsEnabled ? "site-off" : "self-host-off";
    const api = yield* provide(readApiView(actor, { ...scope, window: "24h" }));
    const errors = yield* provide(readErrorsView(actor, { ...scope, window: "24h" }));
    expect([api, errors]).toEqual([{ gate: closed }, { gate: closed }]);
  }).pipe(Effect.scoped));
}

it.live("a hosted Organization without Production reads none of the three views, though its Environment counts", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ polar: hostedPolar, analyticsEnabled: false }, true);
  const views = yield* provide(Effect.all([
    readAnalyticsView(actor, { ...scope, range: "30d" }),
    readApiView(actor, { ...scope, window: "24h" }),
    readErrorsView(actor, { ...scope, window: "24h" }),
  ]));
  expect(views).toEqual([{ gate: "production" }, { gate: "production" }, { gate: "production" }]);
}).pipe(Effect.scoped));

it.live("a hosted Organization with Production reads its views, with ANALYTICS_ENABLED unset", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ polar: hostedPolar, analyticsEnabled: false }, true, true);
  const web = yield* provide(readAnalyticsView(actor, { ...scope, range: "30d" }));
  const [api, errors] = yield* provide(Effect.all([
    readApiView(actor, { ...scope, window: "24h" }),
    readErrorsView(actor, { ...scope, window: "24h" }),
  ]));
  expect([web.gate, api.gate, errors.gate]).toEqual(["on", "on", "on"]);
}).pipe(Effect.scoped));

it.live("a hosted Organization with Production still sees analytics off in an Environment that counts nothing", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ polar: hostedPolar, analyticsEnabled: false }, false, true);
  expect(yield* provide(readAnalyticsView(actor, { ...scope, range: "30d" }))).toEqual({ gate: "site-off" });
}).pipe(Effect.scoped));

it.live("a Cloud with no ClickHouse names what to set up on all three views, before offering to turn the Environment on", () => Effect.gen(function* () {
  const { provide, actor } = yield* setup({ analyticsEnabled: true, clickhouse: false }, false);
  const views = yield* provide(Effect.all([
    readAnalyticsView(actor, { ...scope, range: "30d" }),
    readApiView(actor, { ...scope, window: "24h" }),
    readErrorsView(actor, { ...scope, window: "24h" }),
  ]));
  expect(views).toEqual([{ gate: "no-clickhouse" }, { gate: "no-clickhouse" }, { gate: "no-clickhouse" }]);
}).pipe(Effect.scoped));
