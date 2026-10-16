import { it } from "@effect/vitest";
import { Effect, Exit, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/clickhouse/rollup";
import * as clickHouseStore from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, drainOutbox, enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { dayEnd, dayStart, localDay } from "#/modules/web-analytics/local-time";
import { ensurePartitions, forgetPartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import * as postgresStore from "#/modules/web-analytics/postgres/store";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const HOUR = 3_600_000;
const SHOP = "00000000-0000-4000-8000-00000000c6a1";

const row = (ts: Date, visitorId: string): PageviewRecord => ({
  ts: ts.toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null, utmSource: null,
  utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
});

const failingAtCommand = (real: ClickHouseService, step: number): ClickHouseService => {
  let commands = 0;
  return {
    ...real,
    unchecked: {
      ...real.unchecked,
      command: (query, params) =>
        ++commands === step ? Effect.fail(new ClickHouseFailure({ message: "injected", cause: null })) : real.unchecked.command(query, params),
    },
  };
};

const writesPartOfDaily = (real: ClickHouseService): ClickHouseService => ({
  ...real,
  unchecked: {
    ...real.unchecked,
    command: (query, params) => query.trim().startsWith("INSERT INTO daily")
      ? real.unchecked.command(`${query} HAVING dim = ''`, params).pipe(Effect.andThen(Effect.fail(new ClickHouseFailure({ message: "injected", cause: null }))))
      : real.unchecked.command(query, params),
  },
});

const setup = Effect.gen(function* () {
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const { clickhouse } = yield* clickhouseTestDatabase;
  const provideWith = (service: ClickHouseService) => <A, E, R>(effect: Effect.Effect<A, E, R>) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  return { clickhouse, provide: provideWith(clickhouse), provideWith };
});

const lateRowOnClosedDay = Effect.gen(function* () {
  const { clickhouse, provide, provideWith } = yield* setup;
  const today = Math.floor(Date.now() / DAY) * DAY;
  const now = new Date(today + 12 * HOUR);
  const day = localDay("UTC", today - 3 * DAY);
  const query: postgresStore.StoreQuery = { environmentId: SHOP, timezone: "UTC", range: { start: dayStart("UTC", day), end: dayEnd("UTC", day) }, filters: [] };
  const reads = (store: typeof clickHouseStore | typeof postgresStore) => Effect.all([store.dailyTotals(query), store.hourlyTotals(query), store.breakdown(query, "page")]);

  yield* provide(seedSite({ environmentId: SHOP, timezone: "UTC" }, [row(new Date(today - 3 * DAY + 10 * HOUR), "1"), row(new Date(today - 3 * DAY + 10 * HOUR), "2")]));
  yield* provide(backfillSite(clickhouse, SHOP));
  yield* provide(closeDays(SHOP, now));
  yield* provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const late = row(new Date(today - 3 * DAY + 11 * HOUR), "3");
    yield* ensurePartitions([monthOf(new Date(late.ts))]);
    const inserted = yield* drizzle.insert(analyticsPageview).values(pageviewValues(SHOP, [late])).returning();
    yield* enqueueForClickHouse(inserted);
    yield* drainOutbox(clickhouse);
  }));
  const before = yield* provide(reads(clickHouseStore));
  const truth = yield* provide(reads(postgresStore));
  expect(before).not.toEqual(truth);
  return { clickhouse, provide, provideWith, reads, before, truth };
});

it.live("a rebuild that fails part way leaves readers on the day's previous totals, and a rebuild that completes replaces them once", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, provideWith, reads, before, truth } = yield* lateRowOnClosedDay;

    let failed = 0;
    for (let step = 1; ; step++) {
      const exit = yield* provideWith(failingAtCommand(clickhouse, step))(Effect.exit(rebuildDirtyDays(SHOP)));
      if (Exit.isSuccess(exit)) break;
      failed++;
      expect([before, truth]).toContainEqual(yield* provide(reads(clickHouseStore)));
    }
    expect(failed).toBeGreaterThan(0);
    expect(yield* provide(reads(clickHouseStore))).toEqual(truth);

    yield* provide(Effect.flatMap(pin(), (fence) => collectGarbage(clickhouse, fence)));
    expect(yield* provide(reads(clickHouseStore))).toEqual(truth);
    const [daily] = yield* clickhouse.unchecked.select<{ n: number }>(
      "SELECT count() AS n FROM daily WHERE environment_id = {environmentId:String} AND dim = '' AND value = ''", { environmentId: SHOP });
    const [hourly] = yield* clickhouse.unchecked.select<{ n: number }>(
      "SELECT count() AS n FROM session_hourly WHERE environment_id = {environmentId:String}", { environmentId: SHOP });
    expect([daily?.n, hourly?.n]).toEqual([1, 2]);
  }), 300_000);

it.live("a rebuild that dies after writing only part of the day's daily rows leaves totals, hours and breakdowns on the previous numbers", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, provideWith, reads, before, truth } = yield* lateRowOnClosedDay;

    expect(yield* provideWith(writesPartOfDaily(clickhouse))(Effect.flip(rebuildDirtyDays(SHOP)))).toBeInstanceOf(ClickHouseFailure);
    expect(yield* provide(reads(clickHouseStore))).toEqual(before);

    yield* provide(rebuildDirtyDays(SHOP));
    expect(yield* provide(reads(clickHouseStore))).toEqual(truth);
  }), 300_000);
