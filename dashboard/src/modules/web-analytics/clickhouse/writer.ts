import { sql } from "drizzle-orm";
import { Effect, Schedule } from "effect";
import { Database } from "#/server/database.server";
import { ENTRY_LOOKBACK_SECONDS, localDays, SESSION_GAP_SECONDS } from "../local-time";
import { analyticsOutbox, analyticsPageview } from "../tables";
import type { ClickHouseService } from "./client";
import { TTL_DAYS } from "./schema";
import { binder, newestPublished, type Pin } from "./store";

type Pageview = typeof analyticsPageview.$inferSelect;
type ClickHouseRow = ReturnType<typeof clickHouseRow>;

export const DRAIN_BATCHES = 200;
export const DRAINS_PER_RUN = 30;
const BACKFILL_PAGE = 50_000;

const deduplicationToken = (outboxId: string) => `outbox-${outboxId}`;

export const clickHouseRow = (row: Pageview) => ({
  environment_id: row.environmentId,
  ts: row.ts.toISOString().replace("T", " ").replace("Z", ""),
  visitor_id: row.visitorId.toString(),
  host: row.host,
  pathname: row.pathname,
  source: row.source,
  referrer_host: row.referrerHost,
  utm_source: row.utmSource,
  utm_medium: row.utmMedium,
  utm_campaign: row.utmCampaign,
  utm_term: row.utmTerm,
  utm_content: row.utmContent,
  ref: row.ref,
  language: row.language,
  flags: row.flags,
  retention_days: TTL_DAYS,
});

export const rollupDaysChangedBy = (timezone: string, ts: Date) =>
  localDays(timezone, new Date(ts.getTime() - SESSION_GAP_SECONDS * 1000), new Date(ts.getTime() + ENTRY_LOOKBACK_SECONDS * 1000 + 1));

export const enqueueForClickHouse = Effect.fn("WebAnalytics.enqueueForClickHouse")(function* (rows: ReadonlyArray<Pageview>) {
  const byEnvironment = new Map<string, Array<ClickHouseRow>>();
  for (const row of rows) {
    const batch = byEnvironment.get(row.environmentId) ?? [];
    batch.push(clickHouseRow(row));
    byEnvironment.set(row.environmentId, batch);
  }
  if (byEnvironment.size === 0) return;
  const { drizzle } = yield* Database;
  yield* drizzle.insert(analyticsOutbox).values([...byEnvironment].map(([environmentId, batch]) => ({ environmentId, rows: batch })));
});

export const drainOutbox = Effect.fn("WebAnalytics.drainOutbox")(function* (clickhouse: ClickHouseService) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const batches = yield* drizzle.execute<{ id: string; environment_id: string; rows: ReadonlyArray<ClickHouseRow> }>(sql`
      select id::text as id, environment_id, rows from analytics.outbox order by id limit ${DRAIN_BATCHES} for update skip locked`, "objects");
    if (batches.length === 0) return 0;
    const environments = [...new Set(batches.map((batch) => batch.environment_id))].sort();
    yield* drizzle.execute(sql`
      insert into analytics.clickhouse_site (environment_id)
      select environment_id from analytics.site where environment_id = any(${sql.param(environments)}::text[])
      on conflict do nothing`);
    const sites = yield* drizzle.execute<{ environment_id: string; timezone: string; rolled_up_to: string | null; copy: string }>(sql`
      select c.environment_id, s.timezone, c.rolled_up_to::text as rolled_up_to, c.copy::text as copy
      from analytics.clickhouse_site c join analytics.site s using (environment_id)
      where c.environment_id = any(${sql.param(environments)}::text[])
      order by c.environment_id for update of c`, "objects");
    const byEnvironment = new Map(sites.map((site) => [site.environment_id, site]));
    const dirty = new Map<string, Set<string>>();
    for (const batch of batches) {
      const site = byEnvironment.get(batch.environment_id);
      if (site === undefined) continue;
      const copy = Number(site.copy);
      yield* clickhouse.insert("pageview", batch.rows.map((row) => ({ ...row, copy })), deduplicationToken(batch.id));
      const closed = site.rolled_up_to;
      if (closed === null) continue;
      const days = dirty.get(site.environment_id) ?? new Set<string>();
      for (const row of batch.rows) {
        for (const day of rollupDaysChangedBy(site.timezone, new Date(`${row.ts.replace(" ", "T")}Z`))) if (day <= closed) days.add(day);
      }
      dirty.set(site.environment_id, days);
    }
    for (const [environmentId, days] of dirty) {
      if (days.size === 0) continue;
      yield* drizzle.execute(sql`
        update analytics.clickhouse_site
        set dirty = array(select distinct d from unnest(dirty || ${sql.param([...days])}::date[]) d order by d), updated_at = now()
        where environment_id = ${environmentId}::text`);
    }
    yield* drizzle.execute(sql`delete from analytics.outbox where id = any(${sql.param(batches.map((batch) => batch.id))}::bigint[])`);
    return batches.length;
  }));
});

const demoteReads = Effect.fn("WebAnalytics.demoteReads")(function* (environmentId: string) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      insert into analytics.clickhouse_site (environment_id)
      select environment_id from analytics.site where environment_id = ${environmentId}::text
      on conflict do nothing`);
    return yield* drizzle.execute(sql`
      update analytics.clickhouse_site set reads = false, backfilled_at = null, updated_at = now()
      where environment_id = ${environmentId}::text returning environment_id`, "objects");
  }));
});

export const backfillSite = Effect.fn("WebAnalytics.backfillSite")(function* (clickhouse: ClickHouseService, environmentId: string) {
  const database = yield* Database;
  const demoted = yield* demoteReads(environmentId);
  if (demoted.length === 0) return 0;
  const copy = database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const queued = yield* drizzle.execute<{ id: string }>(sql`
      select id::text as id from analytics.outbox where environment_id = ${environmentId}::text order by id for update`, "objects");
    const [site] = yield* drizzle.execute<{ copy: string }>(sql`
      select copy::text as copy from analytics.clickhouse_site where environment_id = ${environmentId}::text for update`, "objects");
    if (site === undefined) return 0;
    yield* drizzle.execute(sql`delete from analytics.outbox where id = any(${sql.param(queued.map((row) => row.id))}::bigint[])`);
    const copy = Number(site.copy) + 1;
    const params = { environmentId, copy: Number(site.copy) };
    const unpublished = "environment_id = {environmentId:String} AND copy > {copy:UInt64}";
    const [left] = yield* clickhouse.query<{ n: number }>(`SELECT count() AS n FROM pageview WHERE ${unpublished}`, params);
    if (left !== undefined && left.n > 0) yield* clickhouse.command(`DELETE FROM pageview WHERE ${unpublished}`, params);
    let copied = 0;
    let after: Pageview | undefined;
    for (;;) {
      const page: ReadonlyArray<Pageview> = yield* drizzle.select().from(analyticsPageview)
        .where(after === undefined
          ? sql`${analyticsPageview.environmentId} = ${environmentId}`
          : sql`${analyticsPageview.environmentId} = ${environmentId} and (${analyticsPageview.ts}, ${analyticsPageview.visitorId}, ${analyticsPageview.host}, ${analyticsPageview.pathname}) > (${after.ts.toISOString()}::timestamptz, ${after.visitorId.toString()}::bigint, ${after.host}, ${after.pathname})`)
        .orderBy(analyticsPageview.ts, analyticsPageview.visitorId, analyticsPageview.host, analyticsPageview.pathname)
        .limit(BACKFILL_PAGE);
      yield* clickhouse.insert("pageview", page.map((row) => ({ ...clickHouseRow(row), copy })), null);
      copied += page.length;
      after = page[page.length - 1];
      if (page.length < BACKFILL_PAGE) break;
    }
    yield* drizzle.execute(sql`
      update analytics.clickhouse_site
      set copy = ${copy}, floor = generation, rolled_up_to = null, dirty = '{}', reads = false, backfilled_at = now(), updated_at = now()
      where environment_id = ${environmentId}::text`);
    return copied;
  }), { isolationLevel: "repeatable read" });
  return yield* copy.pipe(Effect.retry({ times: 3, schedule: Schedule.exponential("1 second") }));
});

export const ensureBackfilled = Effect.fn("WebAnalytics.ensureBackfilled")(function* (clickhouse: ClickHouseService, environmentId: string) {
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.execute<{ backfilled: boolean }>(sql`
    select backfilled_at is not null as backfilled from analytics.clickhouse_site where environment_id = ${environmentId}::text`, "objects");
  if (site?.backfilled) return null;
  return yield* backfillSite(clickhouse, environmentId);
});

export const forgetDeletedSites = Effect.fn("WebAnalytics.forgetDeletedSites")(function* (clickhouse: ClickHouseService) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const gone = yield* drizzle.execute<{ environment_id: string }>(sql`
      select environment_id from analytics.clickhouse_site c
      where not exists (select 1 from analytics.site s where s.environment_id = c.environment_id)
      order by environment_id for update skip locked`, "objects");
    for (const { environment_id: environmentId } of gone) {
      for (const table of ["pageview", "daily", "session_hourly"]) {
        yield* clickhouse.command(`DELETE FROM ${table} WHERE environment_id = {environmentId:String}`, { environmentId });
      }
    }
    yield* drizzle.execute(sql`
      delete from analytics.clickhouse_site where environment_id = any(${sql.param(gone.map((row) => row.environment_id))}::text[])`);
    return gone.length;
  }));
});

export type Pinned = { readonly environmentId: string; readonly timezone: string } & Pin;

export const publishedPins = Effect.fn("WebAnalytics.publishedPins")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ environment_id: string; timezone: string; copy: string; floor: string; generation: string }>(sql`
    select c.environment_id, s.timezone, c.copy::text as copy, c.floor::text as floor, c.generation::text as generation
    from analytics.clickhouse_site c join analytics.site s using (environment_id) order by c.environment_id`, "objects");
  return rows.map((row): Pinned => ({
    environmentId: row.environment_id,
    timezone: row.timezone,
    copy: Number(row.copy),
    floor: Number(row.floor),
    generation: Number(row.generation),
  }));
});

export const collectGarbage = Effect.fn("WebAnalytics.collectGarbage")(function* (clickhouse: ClickHouseService, pins: ReadonlyArray<Pinned>) {
  let collected = 0;
  for (const pin of pins) {
    const b = binder(pin, pin);
    const superseded = `environment_id = ${b.env} AND generation <= ${b.generation} AND (generation <= ${b.floor} OR NOT ${newestPublished(b, "")})`;
    for (const [table, where] of [
      ["pageview", `environment_id = ${b.env} AND copy < ${b.copy}`],
      ["daily", superseded],
      ["session_hourly", superseded],
    ] as const) {
      const [row] = yield* clickhouse.query<{ n: number }>(`SELECT count() AS n FROM ${table} WHERE ${where}`, b.params);
      if (row === undefined || row.n === 0) continue;
      yield* clickhouse.command(`DELETE FROM ${table} WHERE ${where}`, b.params);
      collected += row.n;
    }
  }
  return collected;
});
