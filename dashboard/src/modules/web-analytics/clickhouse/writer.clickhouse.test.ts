import { sql } from "drizzle-orm";
import { Effect, Exit, Scope } from "effect";
import { afterAll, beforeAll, beforeEach, expect, it } from "vitest";
import { collectGarbage, pin } from "#/modules/web-analytics/clickhouse/attempt";
import { ClickHouseFailure, type ClickHouseService } from "#/modules/web-analytics/clickhouse/client";
import { backfillSite, drainOutbox, enqueueForClickHouse, forgetDeletedSites } from "#/modules/web-analytics/clickhouse/writer";
import { ensurePartitions } from "#/modules/web-analytics/postgres/partitions";
import { analyticsClickhouseSite, analyticsOutbox, analyticsPageview, analyticsSite } from "#/modules/web-analytics/tables";
import { clickhouseTestDatabase } from "#/test/clickhouse";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c401";
const SHOP = "00000000-0000-4000-8000-00000000c403";
const GONE = "00000000-0000-4000-8000-00000000c404";

let harness: PostgresTestHarness;
let clickhouse: ClickHouseService;
const scope = Scope.makeUnsafe();

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  clickhouse = (await Effect.runPromise(clickhouseTestDatabase.pipe(Scope.provide(scope)))).clickhouse;
  await harness.runEffect(ensurePartitions(["2026-10"]));
});

afterAll(async () => {
  await Effect.runPromise(Scope.close(scope, Exit.void));
  await harness?.stop();
});

beforeEach(async () => {
  await harness.db.execute(sql`truncate analytics.site, analytics.pageview, analytics.outbox, analytics.clickhouse_site`);
  for (const table of ["pageview", "daily", "session_hourly"]) await Effect.runPromise(clickhouse.unchecked.command(`TRUNCATE TABLE ${table}`));
  await harness.db.insert(analyticsSite).values([
    { organizationId: ORGANIZATION, environmentId: SHOP, enabled: true, timezone: "Australia/Sydney" },
  ]);
});

const pageview = (environmentId: string, minute: number, pathname = "/", ts = new Date(Date.UTC(2026, 9, 10, 0, minute))) => ({
  environmentId,
  ts,
  visitorId: BigInt(minute % 7),
  host: "shop.example",
  pathname,
  source: "direct",
  referrerHost: null,
  utmSource: null,
  utmMedium: null,
  utmCampaign: null,
  utmTerm: null,
  utmContent: null,
  ref: null,
  language: "en",
  flags: 0,
});

const ingest = (rows: ReadonlyArray<ReturnType<typeof pageview>>) =>
  harness.runTransaction((drizzle) => Effect.gen(function* () {
    const inserted = yield* drizzle.insert(analyticsPageview).values([...rows]).returning();
    yield* enqueueForClickHouse(inserted);
  }));

const storedCount = async (environmentId: string) => {
  const [row] = await Effect.runPromise(clickhouse.query<{ n: number }>(
    "SELECT count() AS n FROM pageview WHERE environment_id = {environmentId:String}", { environmentId }));
  return row?.n;
};

const clickhouseCount = async (environmentId: string) => {
  const [site] = await harness.db.select({ floor: analyticsClickhouseSite.floor, below: analyticsClickhouseSite.below, pending: analyticsClickhouseSite.pending })
    .from(analyticsClickhouseSite).where(sql`${analyticsClickhouseSite.environmentId} = ${environmentId}`);
  const [row] = await Effect.runPromise(clickhouse.query<{ n: number }>(
    "SELECT count() AS n FROM pageview WHERE environment_id = {environmentId:String} AND attempt >= {floor:UInt64} AND attempt < {below:UInt64} AND NOT has({pending:Array(UInt64)}, attempt)",
    { environmentId, floor: Number(site?.floor ?? 0), below: Number(site?.below ?? 0), pending: (site?.pending ?? []).map(Number) }));
  return row?.n;
};

const collect = () => harness.runEffect(Effect.flatMap(pin(), (fence) => collectGarbage(clickhouse, fence)));

const crashingAfterInsert = (real: ClickHouseService, crash: () => boolean): ClickHouseService => ({
  ...real,
  unchecked: {
    ...real.unchecked,
    insert: (table, rows) => real.unchecked.insert(table, rows).pipe(Effect.andThen(Effect.suspend(() =>
      rows.length > 0 && crash() ? Effect.fail(new ClickHouseFailure({ message: "killed", cause: null })) : Effect.void))),
  },
});

const outboxSize = async () => (await harness.db.select().from(analyticsOutbox)).length;

const drain = (service: ClickHouseService = clickhouse) => harness.runEffect(drainOutbox(service));

it("a drain copies every queued pageview to ClickHouse once and empties the outbox", async () => {
  await ingest([pageview(SHOP, 1), pageview(SHOP, 2)]);
  await ingest([pageview(SHOP, 3)]);

  expect(await drain()).toBe(2);

  expect(await clickhouseCount(SHOP)).toBe(3);
  expect(await outboxSize()).toBe(0);
  const [stored] = await Effect.runPromise(clickhouse.query<{ ts: string; visitor_id: number; pathname: string }>(
    "SELECT ts, visitor_id, pathname FROM pageview ORDER BY ts LIMIT 1"));
  expect(stored).toEqual({ ts: "2026-10-10T00:01:00.000Z", visitor_id: 1, pathname: "/" });
  expect(await drain()).toBe(0);
});

it("a writer killed after its ClickHouse insert leaves the batch queued and its rows unpublished, the replay lands them once, and collection drops the orphan", async () => {
  await ingest([pageview(SHOP, 1), pageview(SHOP, 2), pageview(SHOP, 3)]);

  await expect(drain(crashingAfterInsert(clickhouse, () => true))).rejects.toThrow();
  expect(await outboxSize()).toBe(1);
  expect([await storedCount(SHOP), await clickhouseCount(SHOP)]).toEqual([3, 0]);

  expect(await drain()).toBe(1);
  expect([await storedCount(SHOP), await clickhouseCount(SHOP)]).toEqual([6, 3]);
  expect(await outboxSize()).toBe(0);

  expect(await collect()).toBe(3);
  expect([await storedCount(SHOP), await clickhouseCount(SHOP)]).toEqual([3, 3]);
});

it("a drain drops batches whose environment has no site", async () => {
  await ingest([pageview(GONE, 1)]);

  expect(await drain()).toBe(1);

  expect(await clickhouseCount(GONE)).toBe(0);
  expect(await outboxSize()).toBe(0);
  expect(await harness.db.select().from(analyticsClickhouseSite)).toEqual([]);
});

it("a late pageview marks the ClickHouse rollup's closed days dirty, and leaves open days alone", async () => {
  await ingest([pageview(SHOP, 1)]);
  await drain();
  await harness.db.update(analyticsClickhouseSite).set({ rolledUpTo: "2026-10-10" });

  await ingest([pageview(SHOP, 30)]);
  await drain();

  const [site] = await harness.db.select().from(analyticsClickhouseSite);
  expect(site?.dirty).toEqual(["2026-10-10"]);
});

it("a backfill replaces the site's ClickHouse copy with exactly what Postgres holds, and restarts its rollup", async () => {
  await ingest([pageview(SHOP, 1), pageview(SHOP, 2)]);
  await drain();
  await ingest([pageview(SHOP, 3)]);
  await harness.db.insert(analyticsPageview).values([pageview(SHOP, 4), pageview(SHOP, 5, "/docs")]);
  await harness.db.update(analyticsClickhouseSite).set({ rolledUpTo: "2026-10-09", dirty: ["2026-10-08"], reads: true });

  expect(await harness.runEffect(backfillSite(clickhouse, SHOP))).toBe(5);

  expect(await clickhouseCount(SHOP)).toBe(5);
  expect(await outboxSize()).toBe(0);
  expect(await drain()).toBe(0);
  const [site] = await harness.db.select().from(analyticsClickhouseSite);
  expect(site).toMatchObject({ rolledUpTo: null, dirty: [], reads: false, backfilledAt: expect.any(Date) });
});

it("a repeated backfill lands the same rows again instead of losing them to block deduplication", async () => {
  await ingest([pageview(SHOP, 1), pageview(SHOP, 2), pageview(SHOP, 3)]);

  expect(await harness.runEffect(backfillSite(clickhouse, SHOP))).toBe(3);
  expect(await harness.runEffect(backfillSite(clickhouse, SHOP))).toBe(3);

  expect(await clickhouseCount(SHOP)).toBe(3);
});

it("garbage collection drops the raw rows of superseded copies and keeps the published one", async () => {
  await ingest([pageview(SHOP, 1), pageview(SHOP, 2)]);
  await drain();
  await harness.runEffect(backfillSite(clickhouse, SHOP));
  await harness.runEffect(backfillSite(clickhouse, SHOP));
  expect(await storedCount(SHOP)).toBe(6);

  expect(await collect()).toBe(4);

  expect([await storedCount(SHOP), await clickhouseCount(SHOP)]).toEqual([2, 2]);
  expect(await collect()).toBe(0);
});

it("a backfill killed after its insert retries into exactly Postgres's rows", async () => {
  await ingest([pageview(SHOP, 1), pageview(SHOP, 2), pageview(SHOP, 3)]);
  let killed = false;
  const killedOnce = crashingAfterInsert(clickhouse, () => {
    if (killed) return false;
    killed = true;
    return true;
  });

  expect(await harness.runEffect(backfillSite(killedOnce, SHOP))).toBe(3);

  expect([await storedCount(SHOP), await clickhouseCount(SHOP)]).toEqual([6, 3]);
  expect(await collect()).toBe(3);
  expect([await storedCount(SHOP), await clickhouseCount(SHOP)]).toEqual([3, 3]);
});

it("a deleted environment's ClickHouse copy is forgotten", async () => {
  await ingest([pageview(SHOP, 1)]);
  await drain();
  await harness.db.execute(sql`delete from analytics.site where environment_id = ${SHOP}`);

  expect(await harness.runEffect(forgetDeletedSites(clickhouse))).toBe(1);

  expect(await storedCount(SHOP)).toBe(0);
  expect(await harness.db.select().from(analyticsClickhouseSite)).toEqual([]);
});

it("raw pageviews are partitioned by retention and month, so expiry drops whole parts instead of rewriting them", async () => {
  const [table] = await Effect.runPromise(clickhouse.query<{ partition_key: string; engine: string; create_table_query: string }>(
    "SELECT partition_key, engine, create_table_query FROM system.tables WHERE database = currentDatabase() AND name = 'pageview'"));
  expect(table).toMatchObject({ partition_key: "(retention_days, toYYYYMM(ts))", engine: "MergeTree" });
  expect(table?.create_table_query).toContain("ttl_only_drop_parts = 1");
});
