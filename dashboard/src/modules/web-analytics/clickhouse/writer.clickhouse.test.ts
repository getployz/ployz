import { sql } from "drizzle-orm";
import { Effect, Exit, Scope } from "effect";
import { afterAll, beforeAll, beforeEach, expect, it } from "vitest";
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
  for (const table of ["pageview", "daily", "session_hourly"]) await Effect.runPromise(clickhouse.command(`TRUNCATE TABLE ${table}`));
  await harness.db.insert(analyticsSite).values([
    { organizationId: ORGANIZATION, environmentId: SHOP, enabled: true, timezone: "Australia/Sydney" },
  ]);
});

const pageview = (environmentId: string, minute: number, pathname = "/") => ({
  environmentId,
  ts: new Date(Date.UTC(2026, 9, 10, 0, minute)),
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

/** What ingest does for one push: the rows land in Postgres and in the outbox in one transaction. */
const ingest = (rows: ReadonlyArray<ReturnType<typeof pageview>>) =>
  harness.runTransaction((drizzle) => Effect.gen(function* () {
    const inserted = yield* drizzle.insert(analyticsPageview).values([...rows]).returning();
    yield* enqueueForClickHouse(inserted);
  }));

const clickhouseCount = async (environmentId: string) => {
  const [row] = await Effect.runPromise(clickhouse.query<{ n: number }>(
    "SELECT count() AS n FROM pageview WHERE environment_id = {environmentId:String}", { environmentId }));
  return row?.n;
};

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

it("a writer killed after its ClickHouse insert leaves the batch queued, and the replay lands it once", async () => {
  await ingest([pageview(SHOP, 1), pageview(SHOP, 2), pageview(SHOP, 3)]);
  const crashesAfterInsert: ClickHouseService = {
    ...clickhouse,
    insert: (table, rows, token) => clickhouse.insert(table, rows, token).pipe(
      Effect.andThen(Effect.fail(new ClickHouseFailure({ message: "killed", cause: null })))),
  };

  await expect(drain(crashesAfterInsert)).rejects.toThrow();
  expect(await outboxSize()).toBe(1);
  expect(await clickhouseCount(SHOP)).toBe(3);

  expect(await drain()).toBe(1);
  expect(await clickhouseCount(SHOP)).toBe(3);
  expect(await outboxSize()).toBe(0);
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

  // 00:30 UTC is 11:30 in Sydney on the 10th; its session may start the day before and its entry lookback reaches the 11th.
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
  expect(site).toMatchObject({ rolledUpTo: null, dirty: [], reads: false });
});

it("a deleted environment's ClickHouse copy is forgotten", async () => {
  await ingest([pageview(SHOP, 1)]);
  await drain();
  await harness.db.execute(sql`delete from analytics.site where environment_id = ${SHOP}`);

  expect(await harness.runEffect(forgetDeletedSites(clickhouse))).toBe(1);

  expect(await clickhouseCount(SHOP)).toBe(0);
  expect(await harness.db.select().from(analyticsClickhouseSite)).toEqual([]);
});
