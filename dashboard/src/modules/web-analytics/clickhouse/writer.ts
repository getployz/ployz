import { sql } from "drizzle-orm";
import { Effect, Schedule } from "effect";
import { Database } from "#/server/database.server";
import { ENTRY_LOOKBACK_SECONDS, localDays, SESSION_GAP_SECONDS } from "../local-time";
import { analyticsOutbox, analyticsPageview } from "../tables";
import { capped, deleteSite, mint, PublishRefused, SiteUnavailable } from "./attempt";
import type { ClickHouseService } from "./client";
import { TTL_DAYS } from "./schema";

type Pageview = typeof analyticsPageview.$inferSelect;
type ClickHouseRow = ReturnType<typeof clickHouseRow>;
type Batch = { id: string; environment_id: string; rows: ReadonlyArray<ClickHouseRow> };

export const DRAIN_BATCHES = 200;
export const DRAINS_PER_RUN = 30;
const BACKFILL_PAGE = 50_000;

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

/**
 * Inserts one site's batches under a fresh attempt, then publishes them in the transaction that consumes the outbox rows.
 * The outbox rows go first so a backfill that already consumed them refuses this publication instead of double counting.
 */
const drainSite = Effect.fn("WebAnalytics.drainSite")(function* (clickhouse: ClickHouseService, environmentId: string, batches: ReadonlyArray<Batch>) {
  const writer = (yield* mint(clickhouse, [environmentId], "append")).get(environmentId);
  if (writer === undefined) return false;
  const { attempt } = writer;
  const ids = batches.map((batch) => batch.id);
  const rows = batches.flatMap((batch) => batch.rows);
  yield* writer.rows("pageview", rows);
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const consumed = yield* drizzle.execute<{ id: string }>(sql`
      delete from analytics.outbox where id = any(${sql.param(ids)}::bigint[]) returning id::text as id`, "objects");
    if (consumed.length !== ids.length) return yield* new PublishRefused({ environmentId, attempt });
    const [site] = yield* drizzle.execute<{ timezone: string; rolled_up_to: string | null }>(sql`
      select s.timezone, c.rolled_up_to::text as rolled_up_to
      from analytics.clickhouse_site c join analytics.site s using (environment_id)
      where c.environment_id = ${environmentId}::text for update of c`, "objects");
    if (site === undefined) return yield* new PublishRefused({ environmentId, attempt });
    const closed = site.rolled_up_to;
    const dirtied = closed === null ? [] : rows
      .flatMap((row) => rollupDaysChangedBy(site.timezone, new Date(`${row.ts.replace(" ", "T")}Z`)))
      .filter((day) => day <= closed);
    yield* writer.publish({ kind: "append", dirty: dirtied });
    return true;
  })).pipe(Effect.catchTag("PublishRefused", () => Effect.succeed(false)));
});

export const drainOutbox = Effect.fn("WebAnalytics.drainOutbox")(function* (clickhouse: ClickHouseService) {
  const { drizzle } = yield* Database;
  const batches = yield* drizzle.execute<Batch>(sql`
    select id::text as id, environment_id, rows from analytics.outbox order by id limit ${DRAIN_BATCHES}`, "objects");
  if (batches.length === 0) return 0;
  const byEnvironment = new Map<string, Array<Batch>>();
  for (const batch of batches) {
    const group = byEnvironment.get(batch.environment_id) ?? [];
    group.push(batch);
    byEnvironment.set(batch.environment_id, group);
  }
  let drained = 0;
  for (const [environmentId, group] of [...byEnvironment].sort(([a], [b]) => a.localeCompare(b))) {
    if (yield* drainSite(clickhouse, environmentId, group)) drained += group.length;
  }
  const dropped = yield* drizzle.execute<{ id: string }>(sql`
    delete from analytics.outbox o where o.id = any(${sql.param(batches.map((batch) => batch.id))}::bigint[])
      and not exists (select 1 from analytics.site s where s.environment_id = o.environment_id) returning id::text as id`, "objects");
  return drained + dropped.length;
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
  const copy = Effect.gen(function* () {
    const writer = (yield* mint(clickhouse, [environmentId], "backfill")).get(environmentId);
    if (writer === undefined) return yield* new SiteUnavailable({ environmentId });
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const queued = yield* drizzle.execute<{ id: string }>(sql`
        select id::text as id from analytics.outbox where environment_id = ${environmentId}::text order by id for update`, "objects");
      const [site] = yield* drizzle.execute<{ environment_id: string }>(sql`
        select environment_id from analytics.clickhouse_site where environment_id = ${environmentId}::text for update`, "objects");
      if (site === undefined) return 0;
      yield* drizzle.execute(sql`delete from analytics.outbox where id = any(${sql.param(queued.map((row) => row.id))}::bigint[])`);
      let copied = 0;
      let after: Pageview | undefined;
      for (;;) {
        const page: ReadonlyArray<Pageview> = yield* drizzle.select().from(analyticsPageview)
          .where(after === undefined
            ? sql`${analyticsPageview.environmentId} = ${environmentId}`
            : sql`${analyticsPageview.environmentId} = ${environmentId} and (${analyticsPageview.ts}, ${analyticsPageview.visitorId}, ${analyticsPageview.host}, ${analyticsPageview.pathname}) > (${after.ts.toISOString()}::timestamptz, ${after.visitorId.toString()}::bigint, ${after.host}, ${after.pathname})`)
          .orderBy(analyticsPageview.ts, analyticsPageview.visitorId, analyticsPageview.host, analyticsPageview.pathname)
          .limit(BACKFILL_PAGE);
        if (page.length === 0) break;
        yield* writer.rows("pageview", page.map(clickHouseRow));
        copied += page.length;
        after = page[page.length - 1];
        if (page.length < BACKFILL_PAGE) break;
      }
      yield* writer.publish({ kind: "refloor" });
      return copied;
    }), { isolationLevel: "repeatable read" });
  });
  return yield* copy.pipe(Effect.retry({ times: 3, schedule: Schedule.exponential("1 second"), while: (error) => error._tag !== "SiteUnavailable" }));
});

/** A capped site recovers only through a refloor, so the capped state schedules the backfill that publishes one. */
export const ensureBackfilled = Effect.fn("WebAnalytics.ensureBackfilled")(function* (clickhouse: ClickHouseService, environmentId: string) {
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.execute<{ backfilled: boolean }>(sql`
    select backfilled_at is not null as backfilled from analytics.clickhouse_site where environment_id = ${environmentId}::text`, "objects");
  if (site?.backfilled && !(yield* capped(environmentId))) return null;
  return yield* backfillSite(clickhouse, environmentId).pipe(Effect.catchTag("SiteUnavailable", () => Effect.succeed(null)));
});

export const forgetDeletedSites = Effect.fn("WebAnalytics.forgetDeletedSites")(function* (clickhouse: ClickHouseService) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const gone = yield* drizzle.execute<{ environment_id: string }>(sql`
      select environment_id from analytics.clickhouse_site c
      where not exists (select 1 from analytics.site s where s.environment_id = c.environment_id)
      order by environment_id for update skip locked`, "objects");
    for (const { environment_id: environmentId } of gone) yield* deleteSite(clickhouse, environmentId);
    yield* drizzle.execute(sql`
      delete from analytics.clickhouse_site where environment_id = any(${sql.param(gone.map((row) => row.environment_id))}::text[])`);
    return gone.length;
  }));
});
