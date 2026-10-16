import { createClient } from "@clickhouse/client";
import { it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { ClickHouse, ClickHouseFailure, type ClickHouseService, clickHouseClient, makeClickHouse } from "#/modules/web-analytics/clickhouse/client";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/clickhouse/rollup";
import * as store from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, collectGarbage, drainOutbox, enqueueForClickHouse, publishedPins } from "#/modules/web-analytics/clickhouse/writer";
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
  command: (query, params) => real.command(query, params).pipe(Effect.andThen(query.trim().startsWith("INSERT INTO daily")
    ? Effect.fail(new ClickHouseFailure({ message: "after daily insert", cause: null }))
    : Effect.void)),
});

it.live("a timed out rollup INSERT that finishes late cannot add rows to a later rebuild of the day", () =>
  Effect.gen(function* () {
    const { clickhouse, url, provide, pageviews, add } = yield* setup;
    yield* add("3");
    const client = createClient({ url: url.href, request_timeout: 1_000, clickhouse_settings: { output_format_json_quote_64bit_integers: 0 } });
    yield* Effect.addFinalizer(() => Effect.promise(() => client.close()));
    const impatient = makeClickHouse(client);
    const slow: ClickHouseService = {
      ...clickhouse,
      command: (query, params) => query.trim().startsWith("INSERT INTO daily")
        ? impatient.command(`${query} HAVING sleep(3) = 0`, params)
        : clickhouse.command(query, params),
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
    yield* clickhouse.command("ALTER TABLE pageview ADD COLUMN stalled UInt8 DEFAULT sleep(3)");
    let first = true;
    const slow: ClickHouseService = {
      ...clickhouse,
      insert: (table, rows, token) => {
        if (!first) return clickhouse.insert(table, rows, token);
        first = false;
        return impatient.insert(table, rows, token);
      },
    };

    yield* provide(backfillSite(slow, SITE));
    yield* Effect.sleep("3 seconds");

    expect((yield* pageviews)[0]).toBe(2);
  }), 300_000);

it.live("the production client's statements are killed by the server before the client stops waiting", () =>
  Effect.gen(function* () {
    const { url } = yield* setup;
    const production = makeClickHouse(yield* clickHouseClient(url.href));
    const tag = `stalled-${Date.now()}`;
    const stalled = `INSERT INTO complete_day SELECT '${tag}', today(), 1 FROM numbers(1) WHERE sleep(29) = 0
      SETTINGS function_sleep_max_microseconds_per_block = 30000000`;

    const failure = yield* Effect.flip(production.command(stalled));
    const [running] = yield* production.query<{ n: number }>(
      `SELECT count() AS n FROM system.processes WHERE query LIKE '%${tag}%' AND query NOT LIKE '%system.processes%'`);
    const [landed] = yield* production.query<{ n: number }>(`SELECT count() AS n FROM complete_day WHERE environment_id = '${tag}'`);

    expect(failure.cause).toMatchObject({ type: "TIMEOUT_EXCEEDED" });
    expect([running?.n, landed?.n]).toEqual([0, 0]);
  }), 300_000);

it.live("garbage collection removes abandoned attempts below the pin's number and keeps the published ones and later attempts", () =>
  Effect.gen(function* () {
    const { clickhouse, provide, now, reads, add } = yield* setup;
    const distinct = (table: string, column: string) => Effect.map(clickhouse.query<{ id: number }>(
      `SELECT DISTINCT ${column} AS id FROM ${table} WHERE environment_id = {site:String} ORDER BY id`, { site: SITE }),
    (rows) => rows.map(({ id }) => id));
    let killed = false;
    const killedOnce: ClickHouseService = {
      ...clickhouse,
      insert: (table, rows, token) => clickhouse.insert(table, rows, token).pipe(Effect.andThen(Effect.suspend(() => {
        if (killed) return Effect.void;
        killed = true;
        return Effect.fail(new ClickHouseFailure({ message: "killed", cause: null }));
      }))),
    };
    yield* provide(backfillSite(killedOnce, SITE));
    yield* provide(closeDays(SITE, now));
    yield* add("3");
    yield* provide(Effect.exit(rebuildDirtyDays(SITE)), failsAfterDaily(clickhouse));
    yield* provide(rebuildDirtyDays(SITE));
    const pins = yield* provide(publishedPins());
    yield* add("4");
    yield* provide(Effect.exit(rebuildDirtyDays(SITE)), failsAfterDaily(clickhouse));
    const expected = yield* reads;
    const unmarked = (table: string) => Effect.gen(function* () {
      const marked = yield* distinct("complete_day", "generation");
      return (yield* distinct(table, "generation")).filter((generation) => !marked.includes(generation));
    });
    const [abandoned, inFlight] = yield* unmarked("daily");
    expect(yield* unmarked("session_hourly")).toEqual([abandoned, inFlight]);
    expect(abandoned).toBeLessThan(pins[0]?.settledBelow ?? 0);
    expect(inFlight).toBeGreaterThan(pins[0]?.settledBelow ?? Infinity);
    expect(yield* distinct("pageview", "copy")).toHaveLength(3);

    yield* provide(collectGarbage(clickhouse, pins));

    expect(yield* distinct("pageview", "copy")).toEqual([pins[0]?.copy]);
    expect(yield* distinct("complete_day", "generation")).toHaveLength(3);
    expect(yield* unmarked("daily")).toEqual([inFlight]);
    expect(yield* unmarked("session_hourly")).toEqual([inFlight]);
    expect(yield* reads).toEqual(expected);
  }), 300_000);
