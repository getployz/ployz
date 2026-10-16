import { it } from "@effect/vitest";
import { Effect, Fiber, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/clickhouse/rollup";
import * as store from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, drainOutbox, enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const DAY = 86_400_000;
const HOUR = 3_600_000;
const SITE = "00000000-0000-4000-8000-00000000c6a4";

const setup = Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  const today = Math.floor(Date.now() / DAY) * DAY;
  const row = (visitorId: string): PageviewRecord => ({
    ts: new Date(today - 3 * DAY + 10 * HOUR).toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  });
  const query = { environmentId: SITE, timezone: "UTC", range: { start: new Date(today - 3 * DAY), end: new Date(today - 2 * DAY) }, filters: [] };
  const total = (rows: ReadonlyArray<{ pageviews: number }>) => rows.reduce((sum, { pageviews }) => sum + pageviews, 0);
  const daily = provide(Effect.map(store.dailyTotals(query), total));
  const hourly = provide(Effect.map(store.hourlyTotals(query), total));
  const enqueue = (visitorId: string) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(SITE, [row(visitorId)])).returning());
  }));
  yield* provide(seedSite({ environmentId: SITE, timezone: "UTC" }, [row("1"), row("2")]));
  yield* provide(backfillSite(clickhouse, SITE));
  return { clickhouse, provide, today, daily, hourly, enqueue };
});

const failsAfterDaily = (real: ClickHouseService): ClickHouseService => ({
  ...real,
  unchecked: {
    ...real.unchecked,
    command: (query, params) => real.unchecked.command(query, params).pipe(Effect.andThen(query.trim().startsWith("INSERT INTO daily")
      ? Effect.fail(new ClickHouseFailure({ message: "lost rollup acknowledgement", cause: null }))
      : Effect.void)),
  },
});

const sleepsAfterHourly = (real: ClickHouseService): ClickHouseService => {
  let slept = false;
  return {
    ...real,
    unchecked: {
      ...real.unchecked,
      command: (query, params) => real.unchecked.command(query, params).pipe(Effect.andThen(Effect.suspend(() => {
        if (slept || !query.trim().startsWith("INSERT INTO session_hourly")) return Effect.void;
        slept = true;
        return Effect.sleep("4 seconds");
      }))),
    },
  };
};

it.live("a rollup whose acknowledgement was lost stays invisible after a later day publishes and GC runs with a stale pin", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, daily, enqueue } = yield* setup;
    yield* provide(closeDays(SITE, new Date(today - 2 * DAY + HOUR)));
    yield* enqueue("3");
    yield* provide(drainOutbox(clickhouse));
    yield* provide(Effect.flip(rebuildDirtyDays(SITE)), failsAfterDaily(clickhouse));
    expect(yield* daily).toBe(2);
    const fence = yield* provide(pin());
    yield* provide(closeDays(SITE, new Date(today + 12 * HOUR)));
    expect(yield* daily).toBe(2);
    yield* provide(collectGarbage(clickhouse, fence));
    expect(yield* daily).toBe(2);
    yield* provide(rebuildDirtyDays(SITE));
    expect(yield* daily).toBe(3);
  }), 300_000);

it.live("a drain interrupted mid-INSERT cannot land rows inside a rollup", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, today, daily, hourly, enqueue } = yield* setup;
    yield* enqueue("3");
    yield* clickhouse.unchecked.command("ALTER TABLE pageview ADD COLUMN stalled UInt8 DEFAULT sleep(3)");
    const drain = yield* Effect.forkChild(provide(drainOutbox(clickhouse)));
    for (let n = 0; n < 100; n++) {
      const [running] = yield* clickhouse.query<{ n: number }>("SELECT count() AS n FROM system.processes WHERE query LIKE 'INSERT INTO%pageview%'");
      if (running?.n) break;
      yield* Effect.sleep("10 millis");
    }
    yield* Fiber.interrupt(drain);
    yield* provide(closeDays(SITE, new Date(today - 2 * DAY + HOUR)), sleepsAfterHourly(clickhouse));
    expect(yield* daily).toBe(yield* hourly);
    expect(yield* daily).toBe(2);
  }), 300_000);
