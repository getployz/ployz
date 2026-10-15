import { it } from "@effect/vitest";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { ClickHouse, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import * as clickHouseRollup from "#/modules/web-analytics/clickhouse/rollup";
import * as clickHouseStore from "#/modules/web-analytics/clickhouse/store";
import { backfillSite, clickHouseRow } from "#/modules/web-analytics/clickhouse/writer";
import { type Fixture, rangeOf, serialized } from "#/modules/web-analytics/fixtures/fixture";
import type { PageviewRecord } from "#/modules/web-analytics/fixtures/reference";
import { loadFixture, pageviewValues, seedSite } from "#/modules/web-analytics/fixtures/seed";
import { dayStart } from "#/modules/web-analytics/local-time";
import { FILTER_KEYS, type FilterKey, SESSION_DIMS, type SessionDim } from "#/modules/web-analytics/model";
import { ensurePartitions, forgetPartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import * as postgresRollup from "#/modules/web-analytics/postgres/rollup";
import * as postgresStore from "#/modules/web-analytics/postgres/store";
import { analyticsPageview } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { storeTestCloud } from "#/test/store-cloud";

const setup = Effect.fn(function* () {
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ env: { ANALYTICS_ENABLED: "true" } }));
  const { clickhouse } = yield* clickhouseTestDatabase;
  const provide = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
    effect.pipe(Effect.provideService(ClickHouse, clickhouse), Effect.provide(services), Effect.orDie);
  return { provide, clickhouse };
});

const sortedRows = (rows: ReadonlyArray<unknown>) => serialized(rows).map((row) => JSON.stringify(row)).sort();

type Read<Args extends ReadonlyArray<unknown>> = (...args: Args) => Effect.Effect<ReadonlyArray<unknown>, unknown, Database>;
type Reads = {
  readonly dailyTotals: Read<[postgresStore.StoreQuery]>;
  readonly hourlyTotals: Read<[postgresStore.StoreQuery]>;
  readonly breakdown: Read<[postgresStore.StoreQuery, FilterKey]>;
  readonly sessionsBy: Read<[postgresStore.StoreQuery, SessionDim]>;
  readonly entryPages: Read<[postgresStore.StoreQuery]>;
  readonly goalConversions: Read<[postgresStore.StoreQuery]>;
};

/** Every read the store serves, each dim included, with rows sorted so ties ordered differently still compare equal. */
const everyRead = (store: Reads, query: postgresStore.StoreQuery) =>
  Effect.all({
    daily: Effect.map(store.dailyTotals(query), sortedRows),
    hourly: Effect.map(store.hourlyTotals(query), sortedRows),
    breakdown: Effect.forEach(FILTER_KEYS, (dim) => Effect.map(store.breakdown(query, dim), sortedRows)),
    sessions: Effect.forEach(SESSION_DIMS, (dim) => Effect.map(store.sessionsBy(query, dim), sortedRows)),
    entryPages: Effect.map(store.entryPages(query), sortedRows),
    goals: Effect.map(store.goalConversions(query), sortedRows),
  }, { concurrency: 4 });

const queriesOf = (fixture: Fixture) => {
  const distinct = new Map<string, postgresStore.StoreQuery>();
  for (const { spec } of fixture.queries) {
    distinct.set(JSON.stringify([spec.range, spec.filters]), { ...fixture.site, range: rangeOf(spec), filters: spec.filters });
  }
  return [...distinct.entries()];
};

const storesAgree = (queries: ReadonlyArray<readonly [string, postgresStore.StoreQuery]>, when: string) =>
  Effect.forEach(queries, ([name, query]) => Effect.gen(function* () {
    const [postgres, clickhouse] = yield* Effect.all([everyRead(postgresStore, query), everyRead(clickHouseStore, query)], { concurrency: 2 });
    expect(clickhouse, `${name} ${when}`).toEqual(postgres);
  }), { concurrency: 2, discard: true });

const clickHouseState = (environmentId: string) =>
  Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [state] = yield* drizzle.execute<{ rolled_up_to: string | null; dirty: string[] }>(sql`
      select rolled_up_to::text as rolled_up_to, dirty::text[] as dirty from analytics.clickhouse_site
      where environment_id = ${environmentId}::text`, "objects");
    return state;
  });

const rolledUpPageviews = (clickhouse: ClickHouseService, environmentId: string, day: string) =>
  clickhouse.query<{ pageviews: number }>(
    "SELECT sum(pageviews) AS pageviews FROM daily WHERE environment_id = {environmentId:String} AND day = toDate({day:String}) AND dim = ''",
    { environmentId, day },
  ).pipe(Effect.map(([row]) => row?.pageviews ?? 0));

/** Lands a pageview the way a late ingest does, in Postgres and in the ClickHouse copy. */
const insertBoth = (clickhouse: ClickHouseService, environmentId: string, record: PageviewRecord) =>
  Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* ensurePartitions([monthOf(new Date(record.ts))]);
    const [inserted] = yield* drizzle.insert(analyticsPageview).values(pageviewValues(environmentId, [record])).returning();
    if (inserted !== undefined) yield* clickhouse.insert("pageview", [clickHouseRow(inserted)]);
  });

const row = (ts: string, visitorId: string, fields: Partial<PageviewRecord> = {}): PageviewRecord => ({
  ts, visitorId, host: "shop.example", pathname: "/", source: "", referrerHost: null, utmSource: null, utmMedium: null,
  utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en", ...fields,
});

const CLOSED = {
  utc: { first: "2026-10-08", last: "2026-10-14" },
  sydney: { first: "2026-10-02", last: "2026-10-05" },
} as const;

for (const name of ["utc", "sydney"] as const) {
  const fixture = loadFixture(name);
  const { environmentId, timezone } = fixture.site;
  const queries = queriesOf(fixture);

  it.live(`${name}: ClickHouse answers every read like Postgres, raw, rolled up, and after a dirty day is rebuilt`, () =>
    Effect.gen(function* () {
      const { provide, clickhouse } = yield* setup();
      yield* provide(seedSite(fixture.site, fixture.rows, fixture.goals));
      yield* provide(backfillSite(clickhouse, environmentId));
      yield* provide(storesAgree(queries, "raw"));

      const now = new Date(fixture.now);
      const [postgresClosed, clickHouseClosed] = yield* provide(Effect.all([
        postgresRollup.closeDays(environmentId, now),
        clickHouseRollup.closeDays(environmentId, now),
      ]));
      expect(clickHouseClosed).toBe(postgresClosed);
      expect((yield* provide(clickHouseState(environmentId)))?.rolled_up_to).toBe(CLOSED[name].last);
      yield* provide(storesAgree(queries, "rolled up"));

      const day = CLOSED[name].first;
      const before = yield* rolledUpPageviews(clickhouse, environmentId, day);
      const late = row(new Date(dayStart(timezone, day).getTime() + 12 * 3_600_000).toISOString(), "990001", { pathname: "/late", source: "late.example" });
      yield* provide(insertBoth(clickhouse, environmentId, late));
      yield* provide(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        yield* drizzle.execute(sql`update analytics.rollup_state set dirty = array[${day}::date] where environment_id = ${environmentId}::text`);
        yield* drizzle.execute(sql`update analytics.clickhouse_site set dirty = array[${day}::date] where environment_id = ${environmentId}::text`);
      }));
      const [postgresRebuilt, clickHouseRebuilt] = yield* provide(Effect.all([
        postgresRollup.rebuildDirtyDays(environmentId),
        clickHouseRollup.rebuildDirtyDays(environmentId),
      ]));
      expect([postgresRebuilt, clickHouseRebuilt]).toEqual([1, 1]);
      expect((yield* provide(clickHouseState(environmentId)))?.dirty).toEqual([]);
      expect(yield* rolledUpPageviews(clickhouse, environmentId, day)).toBe(before + 1);
      yield* provide(storesAgree(queries, "after the dirty day is rebuilt"));
    }), 300_000);
}

it.live("ClickHouse reads and rollups skip a pageview older than its retention", () =>
  Effect.gen(function* () {
    const { provide, clickhouse } = yield* setup();
    const site = { environmentId: "00000000-0000-4000-8000-00000000c4f1", timezone: "UTC" };
    const now = new Date();
    const recent = row(new Date(now.getTime() - 3 * 86_400_000).toISOString(), "1");
    yield* provide(seedSite(site, [recent]));
    yield* provide(backfillSite(clickhouse, site.environmentId));
    const [expired] = pageviewValues(site.environmentId, [row(new Date(now.getTime() - 500 * 86_400_000).toISOString(), "2", { pathname: "/expired" })]);
    if (expired === undefined) throw new Error("no expired row");
    // An insert drops already-expired rows, so lift the TTL to hold one the way it sits until a TTL merge removes it.
    yield* clickhouse.command("ALTER TABLE pageview REMOVE TTL");
    yield* clickhouse.insert("pageview", [clickHouseRow({ ...expired, flags: 0 })]);

    const query = { ...site, range: { start: new Date(now.getTime() - 600 * 86_400_000), end: now }, filters: [] };
    const reads = yield* provide(everyRead(clickHouseStore, query));
    expect(reads).toEqual(yield* provide(everyRead(postgresStore, query)));
    expect(reads.breakdown[FILTER_KEYS.indexOf("page")]).toEqual([JSON.stringify({ value: "/", pageviews: 1, visitors: 1 })]);

    yield* provide(clickHouseRollup.closeDays(site.environmentId, now));
    expect(yield* rolledUpPageviews(clickhouse, site.environmentId, expired.ts.toISOString().slice(0, 10))).toBe(0);
    expect(yield* rolledUpPageviews(clickhouse, site.environmentId, recent.ts.slice(0, 10))).toBe(1);
  }), 300_000);
