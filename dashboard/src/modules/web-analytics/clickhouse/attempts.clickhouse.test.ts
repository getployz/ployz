import { createClient } from "@clickhouse/client";
import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { collectGarbage, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouse, ClickHouseFailure, type ClickHouseService, clickHouseClient, makeClickHouse } from "#/modules/web-analytics/clickhouse/client";
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
const SITE = "00000000-0000-4000-8000-00000000c6a2";

const setup = Effect.gen(function* () {
  forgetPartitions();
  const { clickhouse, url } = yield* clickhouseTestDatabase;
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>, service: ClickHouseService = clickhouse) =>
    effect.pipe(Effect.provideService(ClickHouse, service), Effect.provide(services), Effect.orDie);
  const today = Math.floor(Date.now() / DAY) * DAY;
  const now = new Date(today + 12 * HOUR);
  const row = (visitorId: string): PageviewRecord => ({
    ts: new Date(today - 3 * DAY + 10 * HOUR).toISOString(), visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null,
    utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en",
  });
  const query = { environmentId: SITE, timezone: "UTC", range: { start: new Date(today - 4 * DAY), end: new Date(today) }, filters: [] };
  yield* provide(seedSite({ environmentId: SITE, timezone: "UTC" }, [row("1"), row("2")]));
  yield* provide(backfillSite(clickhouse, SITE));
  yield* provide(closeDays(SITE, now));
  const reads = provide(Effect.all([store.dailyTotals(query), store.hourlyTotals(query), store.breakdown(query, "page")]));
  const pageviews = Effect.map(reads, ([daily, hourly, pages]) =>
    [daily, hourly, pages].map((rows) => rows.reduce((sum, { pageviews }) => sum + pageviews, 0)));
  const add = (visitorId: string) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* enqueueForClickHouse(yield* drizzle.insert(analyticsPageview).values(pageviewValues(SITE, [row(visitorId)])).returning());
    yield* drainOutbox(clickhouse);
  }));
  return { clickhouse, url, provide, now, reads, pageviews, add };
});

const failsAfterDaily = (real: ClickHouseService): ClickHouseService => ({
  ...real,
  unchecked: {
    ...real.unchecked,
    command: (query, params) => real.unchecked.command(query, params).pipe(Effect.andThen(query.trim().startsWith("INSERT INTO daily")
      ? Effect.fail(new ClickHouseFailure({ message: "after daily insert", cause: null }))
      : Effect.void)),
  },
});

const insertingOnceThrough = (real: ClickHouseService, other: ClickHouseService): ClickHouseService => {
  let first = true;
  return {
    ...real,
    unchecked: {
      ...real.unchecked,
      insert: (table, rows) => {
        if (!first) return real.unchecked.insert(table, rows);
        first = false;
        return other.unchecked.insert(table, rows);
      },
    },
  };
};

const killedOnceAfterInsert = (real: ClickHouseService): ClickHouseService => {
  let killed = false;
  return {
    ...real,
    unchecked: {
      ...real.unchecked,
      insert: (table, rows) => real.unchecked.insert(table, rows).pipe(Effect.andThen(Effect.suspend(() => {
        if (killed || rows.length === 0) return Effect.void;
        killed = true;
        return Effect.fail(new ClickHouseFailure({ message: "killed", cause: null }));
      }))),
    },
  };
};

it.live("a timed out rollup INSERT that finishes late cannot add rows to a later rebuild of the day", () =>
  Effect.gen(function* () {
    const { clickhouse, url, provide, pageviews, add } = yield* setup;
    yield* add("3");
    const client = createClient({ url: url.href, request_timeout: 1_000, clickhouse_settings: { output_format_json_quote_64bit_integers: 0 } });
    yield* Effect.addFinalizer(() => Effect.promise(() => client.close()));
    const impatient = makeClickHouse(client);
    const slow: ClickHouseService = {
      ...clickhouse,
      unchecked: {
        ...clickhouse.unchecked,
        command: (query, params) => query.trim().startsWith("INSERT INTO daily")
          ? impatient.unchecked.command(`${query} HAVING sleep(3) = 0`, params)
          : clickhouse.unchecked.command(query, params),
      },
    };

    expect(yield* provide(Effect.flip(rebuildDirtyDays(SITE)), slow)).toBeInstanceOf(ClickHouseFailure);
    yield* provide(rebuildDirtyDays(SITE));
    yield* Effect.sleep("4 seconds");

    expect(yield* pageviews).toEqual([3, 3, 3]);
  }), 300_000);

it.live("a timed out backfill INSERT that finishes late cannot add rows to the copy the retry publishes", () =>
  Effect.gen(function* () {
    const { clickhouse, url, provide, pageviews } = yield* setup;
    const client = createClient({ url: url.href, request_timeout: 200, clickhouse_settings: { output_format_json_quote_64bit_integers: 0 } });
    yield* Effect.addFinalizer(() => Effect.promise(() => client.close()));
    const impatient = makeClickHouse(client);
    yield* clickhouse.unchecked.command("ALTER TABLE pageview ADD COLUMN stalled UInt8 DEFAULT sleep(3)");

    yield* provide(backfillSite(insertingOnceThrough(clickhouse, impatient), SITE));
    yield* Effect.sleep("3 seconds");

    expect((yield* pageviews)[0]).toBe(2);
  }), 300_000);

it.live("the production client's statements are killed by the server before the client stops waiting", () =>
  Effect.gen(function* () {
    const { url } = yield* setup;
    const production = makeClickHouse(yield* clickHouseClient(url.href));
    const tag = `stalled-${Date.now()}`;
    const stalled = `INSERT INTO daily (environment_id, day, attempt, dim, value, pageviews, visitors, visits, bounces, total_duration)
      SELECT '${tag}', today(), 1, '', '', 1, 1, 1, 0, 0 FROM numbers(1) WHERE sleep(29) = 0
      SETTINGS function_sleep_max_microseconds_per_block = 30000000`;

    const failure = yield* Effect.flip(production.unchecked.command(stalled));
    const [running] = yield* production.unchecked.select<{ n: number }>(
      `SELECT count() AS n FROM system.processes WHERE query LIKE '%${tag}%' AND query NOT LIKE '%system.processes%'`);
    const [landed] = yield* production.unchecked.select<{ n: number }>(`SELECT count() AS n FROM daily WHERE environment_id = '${tag}'`);

    expect(failure.cause).toMatchObject({ type: "TIMEOUT_EXCEEDED" });
    expect([running?.n, landed?.n]).toEqual([0, 0]);
  }), 300_000);

it.live("garbage collection removes abandoned attempts below the fence and keeps the published ones and the attempts still in flight", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, now, reads, add } = yield* setup;
    const attempts = (table: string) => Effect.map(clickhouse.unchecked.select<{ id: number }>(
      `SELECT DISTINCT attempt AS id FROM ${table} WHERE environment_id = {site:String} ORDER BY id`, { site: SITE }),
    (rows) => rows.map(({ id }) => id));
    const published = provide(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [row] = yield* drizzle.execute<{ floor: string; below: string; pending: string[]; days: Record<string, number> }>(sql`
        select floor::text as floor, below::text as below, pending::text[] as pending, days
        from analytics.clickhouse_site where environment_id = ${SITE}::text`, "objects");
      if (row === undefined) return yield* Effect.die("the site has no clickhouse_site row");
      return { floor: Number(row.floor), below: Number(row.below), pending: row.pending.map(Number), days: row.days };
    }));
    const unpublished = (table: string) => Effect.gen(function* () {
      const days = new Set(Object.values((yield* published).days));
      return (yield* attempts(table)).filter((attempt) => !days.has(attempt));
    });
    yield* provide(backfillSite(killedOnceAfterInsert(clickhouse), SITE));
    yield* provide(closeDays(SITE, now));
    yield* add("3");
    yield* provide(Effect.exit(rebuildDirtyDays(SITE)), failsAfterDaily(clickhouse));
    yield* provide(rebuildDirtyDays(SITE));
    const fence = yield* provide(pin());
    yield* add("4");
    yield* provide(Effect.exit(rebuildDirtyDays(SITE)), failsAfterDaily(clickhouse));
    const expected = yield* reads;
    const stale = yield* unpublished("daily");
    expect(yield* unpublished("session_hourly")).toEqual(stale);
    const [superseded, replaced, abandoned, inFlight] = stale;
    expect(stale).toHaveLength(4);
    expect(abandoned).toBeLessThan(fence);
    expect(inFlight).toBeGreaterThan(fence);
    const landed = yield* attempts("pageview");
    expect(landed).toHaveLength(5);

    yield* provide(collectGarbage(clickhouse, fence));

    const { floor, below, pending } = yield* published;
    const visible = Array.from({ length: below - floor }, (_, i) => floor + i).filter((attempt) => !pending.includes(attempt));
    expect(superseded).toBeLessThan(floor);
    expect(replaced).toBeGreaterThan(floor);
    expect(yield* attempts("pageview")).toEqual(landed.filter((attempt) => visible.includes(attempt)));
    expect(yield* attempts("pageview")).toHaveLength(3);
    expect(yield* unpublished("daily")).toEqual([inFlight]);
    expect(yield* unpublished("session_hourly")).toEqual([inFlight]);
    expect(yield* reads).toEqual(expected);
  }), 300_000);
