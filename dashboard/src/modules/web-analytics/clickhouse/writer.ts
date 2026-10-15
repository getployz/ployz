import { sql } from "drizzle-orm";
import { Effect, Schedule } from "effect";
import { Database } from "#/server/database.server";
import { ENTRY_LOOKBACK_SECONDS, localDays, SESSION_GAP_SECONDS } from "../local-time";
import { analyticsOutbox, analyticsPageview } from "../tables";
import type { ClickHouseService } from "./client";
import { RETENTION_DAYS } from "./schema";

type Pageview = typeof analyticsPageview.$inferSelect;
type ClickHouseRow = ReturnType<typeof clickHouseRow>;

export const DRAIN_BATCHES = 200;
const BACKFILL_PAGE = 50_000;

/** A `pageview` row as ClickHouse's JSONEachRow reads it: UTC wall time, and the visitor id quoted so it survives JSON. */
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
  retention_days: RETENTION_DAYS,
});

/** The local days whose rollup a pageview at `ts` can change: its own, the day a session it ends began on, and the next day's entry lookback. */
export const rollupDaysChangedBy = (timezone: string, ts: Date) =>
  localDays(timezone, new Date(ts.getTime() - SESSION_GAP_SECONDS * 1000), new Date(ts.getTime() + ENTRY_LOOKBACK_SECONDS * 1000 + 1));

/** Runs inside ingest's transaction, so a batch reaches the outbox exactly when its rows reach Postgres. */
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

/**
 * Copies the oldest outbox batches to ClickHouse and deletes them in one Postgres transaction. A crash after the
 * ClickHouse insert leaves the batch in the outbox, and its retry carries the same deduplication token, so it lands once.
 * The clickhouse_site rows stay locked across the insert, so a ClickHouse rollup of the same site never reads half a batch.
 */
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
    const sites = yield* drizzle.execute<{ environment_id: string; timezone: string; rolled_up_to: string | null }>(sql`
      select c.environment_id, s.timezone, c.rolled_up_to::text as rolled_up_to
      from analytics.clickhouse_site c join analytics.site s using (environment_id)
      where c.environment_id = any(${sql.param(environments)}::text[])
      order by c.environment_id for update of c`, "objects");
    const byEnvironment = new Map(sites.map((site) => [site.environment_id, site]));
    const dirty = new Map<string, Set<string>>();
    for (const batch of batches) {
      const site = byEnvironment.get(batch.environment_id);
      if (site === undefined) continue;
      yield* clickhouse.insert("pageview", batch.rows, `outbox-${batch.id}`);
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

/**
 * Replaces a site's ClickHouse copy with every pageview Postgres holds for it, and restarts its ClickHouse rollup.
 * One repeatable-read snapshot holds both the outbox batches it deletes and the pageviews it copies, so each row lands
 * once: a later batch is drained on top, an earlier one is replaced. The site's clickhouse_site lock holds the writer
 * off until the copy commits. Reads stay on Postgres until the rollup catches up and a parity check flips them.
 */
export const backfillSite = Effect.fn("WebAnalytics.backfillSite")(function* (clickhouse: ClickHouseService, environmentId: string) {
  const database = yield* Database;
  const copy = database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const queued = yield* drizzle.execute<{ id: string }>(sql`
      select id::text as id from analytics.outbox where environment_id = ${environmentId}::text order by id for update`, "objects");
    yield* drizzle.execute(sql`
      insert into analytics.clickhouse_site (environment_id)
      select environment_id from analytics.site where environment_id = ${environmentId}::text
      on conflict do nothing`);
    const [site] = yield* drizzle.execute<{ environment_id: string }>(sql`
      select environment_id from analytics.clickhouse_site where environment_id = ${environmentId}::text for update`, "objects");
    if (site === undefined) return 0;
    yield* drizzle.execute(sql`delete from analytics.outbox where id = any(${sql.param(queued.map((row) => row.id))}::bigint[])`);
    const params = { environmentId };
    for (const table of ["pageview", "daily", "session_hourly"]) {
      yield* clickhouse.command(`DELETE FROM ${table} WHERE environment_id = {environmentId:String}`, params);
    }
    let copied = 0;
    let after: Pageview | undefined;
    for (;;) {
      const page: ReadonlyArray<Pageview> = yield* drizzle.select().from(analyticsPageview)
        .where(after === undefined
          ? sql`${analyticsPageview.environmentId} = ${environmentId}`
          : sql`${analyticsPageview.environmentId} = ${environmentId} and (${analyticsPageview.ts}, ${analyticsPageview.visitorId}, ${analyticsPageview.host}, ${analyticsPageview.pathname}) > (${after.ts.toISOString()}::timestamptz, ${after.visitorId.toString()}::bigint, ${after.host}, ${after.pathname})`)
        .orderBy(analyticsPageview.ts, analyticsPageview.visitorId, analyticsPageview.host, analyticsPageview.pathname)
        .limit(BACKFILL_PAGE);
      yield* clickhouse.insert("pageview", page.map(clickHouseRow));
      copied += page.length;
      after = page[page.length - 1];
      if (page.length < BACKFILL_PAGE) break;
    }
    yield* drizzle.execute(sql`
      update analytics.clickhouse_site set rolled_up_to = null, dirty = '{}', reads = false, updated_at = now()
      where environment_id = ${environmentId}::text`);
    return copied;
  }), { isolationLevel: "repeatable read" });
  // A writer that drained one of this site's batches after the snapshot began fails the copy's locks; the next attempt sees it gone.
  return yield* copy.pipe(Effect.retry({ times: 3, schedule: Schedule.exponential("1 second") }));
});

/** Drops the ClickHouse copy of every site whose environment was deleted; the delete trigger only reaches Postgres, and the writer drops their queued batches. */
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
