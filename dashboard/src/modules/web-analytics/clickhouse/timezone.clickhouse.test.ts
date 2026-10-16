import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { TestClock } from "effect/testing";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { collectGarbage, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse } from "#/modules/web-analytics/clickhouse/client";
import { promoteReads } from "#/modules/web-analytics/clickhouse/promote";
import { closeDays } from "#/modules/web-analytics/clickhouse/rollup";
import * as clickHouseStore from "#/modules/web-analytics/clickhouse/store";
import { backfillSite } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues } from "#/modules/web-analytics/fixtures/seed";
import { addDays, dayStart, localDay } from "#/modules/web-analytics/local-time";
import { ensurePartitions, forgetPartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import * as postgresStore from "#/modules/web-analytics/postgres/store";
import { setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { analyticsClickhouseSite, analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const HOUR = 3_600_000;
const ORGANIZATION = "00000000-0000-4000-8000-00000000c5a1";
const PROJECT = "00000000-0000-4000-8000-00000000c5a2";
const PRODUCTION = "00000000-0000-4000-8000-00000000c5a3";
const SYDNEY = "Australia/Sydney";

const row = (ts: Date, visitorId: string): PageviewRecord => ({
  ts: ts.toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null, utmSource: null,
  utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
});

const cloud = Effect.gen(function* () {
  forgetPartitions();
  const { url, clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({
    env: { ANALYTICS_ENABLED: "true", CLICKHOUSE_URL: url.href, ANALYTICS_STORE: "clickhouse" },
  }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
    effect.pipe(Effect.provideService(ClickHouse, clickhouse), Effect.provide(services), Effect.orDie);
  yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  const store = yield* provide(cloudStore);
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: PRODUCTION }));
  const today = Math.floor(Date.now() / DAY) * DAY;
  const now = new Date(today + 12 * HOUR);
  yield* TestClock.setTime(now.getTime());
  return { clickhouse, provide, today, now };
});

const state = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.select({ rolledUpTo: analyticsClickhouseSite.rolledUpTo, dirty: analyticsClickhouseSite.dirty, reads: analyticsClickhouseSite.reads })
    .from(analyticsClickhouseSite).where(eq(analyticsClickhouseSite.environmentId, PRODUCTION));
  return site;
});

it.live("changing a promoted site's timezone sends readers back to Postgres, and the rebuilt ClickHouse copy answers on the new timezone's days only", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, now } = yield* cloud;
    const rows = [row(new Date(today - 3 * DAY + 20 * HOUR), "1"), row(new Date(today - 2 * DAY + 20 * HOUR), "2")];
    yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "UTC" }));
    yield* provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* ensurePartitions(rows.map((pageview) => monthOf(new Date(pageview.ts))));
      yield* drizzle.insert(analyticsPageview).values(pageviewValues(PRODUCTION, rows));
    }));
    yield* provide(backfillSite(clickhouse, PRODUCTION));
    yield* provide(closeDays(PRODUCTION, now));
    expect(yield* provide(promoteReads(PRODUCTION))).toBe(true);

    yield* provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: SYDNEY }));
    expect(yield* provide(state)).toEqual({ rolledUpTo: null, dirty: [], reads: false });

    yield* provide(closeDays(PRODUCTION, now));
    expect(yield* provide(promoteReads(PRODUCTION))).toBe(true);

    yield* provide(Effect.flatMap(pin(), (fence) => collectGarbage(clickhouse, fence)));
    const sydneyDays = rows.map((pageview) => localDay(SYDNEY, new Date(pageview.ts).getTime()));
    const days = yield* clickhouse.query<{ day: string }>(
      "SELECT DISTINCT toString(day) AS day FROM daily WHERE environment_id = {environmentId:String} ORDER BY day", { environmentId: PRODUCTION });
    expect(days.map((daily) => daily.day)).toEqual(sydneyDays);
    const query: postgresStore.StoreQuery = {
      environmentId: PRODUCTION,
      timezone: SYDNEY,
      range: { start: dayStart(SYDNEY, addDays(SYDNEY, sydneyDays[0] ?? "", -2)), end: dayStart(SYDNEY, localDay(SYDNEY, now.getTime())) },
      filters: [],
    };
    const [postgres, rebuilt] = yield* provide(Effect.all([postgresStore.dailyTotals(query), clickHouseStore.dailyTotals(query)]));
    expect(rebuilt).toEqual(postgres);
    expect(rebuilt.map((bucket) => bucket.pageviews)).toEqual([1, 1]);
  }).pipe(Effect.provide(TestClock.layer())), 300_000);
