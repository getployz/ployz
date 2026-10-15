import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { addDays, dayEnd, dayStart, localDay, retentionCutoff, SESSION_GAP_SECONDS } from "../local-time";
import { ensurePartitions, monthOf, nextMonth, pageviewPartitions } from "./partitions";
import { instant, localHour, type StoreQuery, visitCtes } from "./store";

export type RollupSite = { readonly environmentId: string; readonly timezone: string };

type LockedSite = RollupSite & { readonly enabled: boolean; readonly rolledUpTo: string | null; readonly dirty: ReadonlyArray<string> };

const MAX_DAYS_PER_SITE_RUN = 31;
const PARTITION_MONTHS_AHEAD = 2;

const siteQuery = (site: RollupSite, start: Date, end: Date): StoreQuery =>
  ({ environmentId: site.environmentId, timezone: site.timezone, range: { start, end }, filters: [] });

/** Holds the site's rollup_state row lock, which ingest and timezone changes also take, so the site read here stays current until commit. */
const withRollupLock = <A, E, R>(environmentId: string, program: (site: LockedSite) => Effect.Effect<A, E, R>, otherwise: A) =>
  Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`
        insert into analytics.rollup_state (environment_id)
        select environment_id from analytics.site where environment_id = ${environmentId}::text
        on conflict do nothing`);
      const [state] = yield* drizzle.execute<{ rolled_up_to: string | null; dirty: string[] }>(sql`
        select rolled_up_to::text as rolled_up_to, dirty::text[] as dirty from analytics.rollup_state
        where environment_id = ${environmentId}::text for update`, "objects");
      const [site] = yield* drizzle.execute<{ timezone: string; enabled: boolean }>(sql`
        select timezone, enabled from analytics.site where environment_id = ${environmentId}::text`, "objects");
      if (state === undefined || site === undefined) return otherwise;
      // A day's sessionization sorts and hashes a few hundred thousand rows; the default 4 MB spills every step to disk.
      yield* drizzle.execute(sql`set local work_mem = '64MB'`);
      return yield* program({ environmentId, timezone: site.timezone, enabled: site.enabled, rolledUpTo: state.rolled_up_to, dirty: state.dirty });
    }));
  });

const PAGE_DIMS = sql.raw(`('', ''), ('page', pathname), ('host', host), ('source', source), ('utm_source', utm_source),
  ('utm_medium', utm_medium), ('utm_campaign', utm_campaign), ('utm_term', utm_term), ('utm_content', utm_content),
  ('language', language)`);
const SESSION_DIMS = sql.raw(`('', ''), ('host', host), ('source', source), ('utm_source', utm_source),
  ('utm_medium', utm_medium), ('utm_campaign', utm_campaign), ('utm_term', utm_term), ('utm_content', utm_content),
  ('language', language), ('entry_page', pathname)`);

const writeDay = Effect.fn("WebAnalytics.writeDay")(function* (site: RollupSite, day: string) {
  const { drizzle } = yield* Database;
  const start = dayStart(site.timezone, day);
  const end = dayEnd(site.timezone, day);
  const query = siteQuery(site, start, end);
  yield* drizzle.execute(sql`
    delete from analytics.daily where environment_id = ${site.environmentId}::text and day = ${day}::date`);
  yield* drizzle.execute(sql`
    delete from analytics.session_hourly
    where environment_id = ${site.environmentId}::text and hour >= ${instant(start)} and hour < ${instant(end)}`);
  yield* drizzle.execute(sql`
    with ${visitCtes(query, { start, end })},
    sessions as materialized (select * from visits where ts >= ${instant(start)} and ts < ${instant(end)}),
    day_rows as materialized (select * from visit_rows),
    hourly as (
      insert into analytics.session_hourly (environment_id, hour, pageviews, visitors, visits, bounces, total_duration)
      select ${site.environmentId}::text, hour, coalesce(pv.pageviews, 0), coalesce(pv.visitors, 0),
        coalesce(ss.visits, 0), coalesce(ss.bounces, 0), coalesce(ss.total_duration, 0)
      from (
        select ${localHour(site.timezone)} as hour, count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors
        from day_rows group by 1
      ) pv
      full join (
        select ${localHour(site.timezone)} as hour, count(*)::bigint as visits, count(*) filter (where pageviews = 1)::bigint as bounces,
          coalesce(sum(duration), 0)::bigint as total_duration
        from sessions group by 1
      ) ss using (hour)
    ), pv as (
      select dim, value, sum(pageviews)::bigint as pageviews, count(*)::bigint as visitors
      from (
        select kv.dim, kv.value, visitor_id, count(*) as pageviews
        from day_rows
        cross join lateral (values ${PAGE_DIMS}) as kv(dim, value)
        where kv.value is not null
        group by kv.dim, kv.value, visitor_id
      ) per_visitor
      group by dim, value
    ), ss as (
      select dim, value, sum(visits)::bigint as visits, sum(bounces)::bigint as bounces,
        sum(total_duration)::bigint as total_duration, count(*)::bigint as visitors
      from (
        select kv.dim, kv.value, visitor_id, count(*) as visits, count(*) filter (where pageviews = 1) as bounces,
          coalesce(sum(duration), 0) as total_duration
        from sessions
        cross join lateral (values ${SESSION_DIMS}) as kv(dim, value)
        where kv.value is not null
        group by kv.dim, kv.value, visitor_id
      ) per_visitor
      group by dim, value
    )
    insert into analytics.daily (environment_id, day, dim, value, pageviews, visitors, visits, bounces, total_duration)
    select ${site.environmentId}::text, ${day}::date, dim, value,
      coalesce(pv.pageviews, 0), case when dim = 'entry_page' then ss.visitors else coalesce(pv.visitors, 0) end,
      coalesce(ss.visits, 0), coalesce(ss.bounces, 0), coalesce(ss.total_duration, 0)
    from pv full join ss using (dim, value)`);
});

const firstDay = Effect.fn("WebAnalytics.firstDay")(function* (site: RollupSite) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ ts: string | null }>(sql`
    select min(ts)::text as ts from analytics.pageview where environment_id = ${site.environmentId}::text`, "objects");
  const ts = rows[0]?.ts;
  return ts == null ? null : localDay(site.timezone, new Date(ts));
});

const closeNextDay = (environmentId: string, now: Date) =>
  withRollupLock(environmentId, (site) => Effect.gen(function* () {
    if (!site.enabled) return false;
    const day = site.rolledUpTo === null ? yield* firstDay(site) : addDays(site.timezone, site.rolledUpTo, 1);
    if (day === null || dayEnd(site.timezone, day).getTime() + SESSION_GAP_SECONDS * 1000 > now.getTime()) return false;
    yield* writeDay(site, day);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      update analytics.rollup_state
      set rolled_up_to = ${day}::date, dirty = array_remove(dirty, ${day}::date), updated_at = now()
      where environment_id = ${environmentId}::text`);
    return true;
  }), false);

const rebuildNextDirty = (environmentId: string) =>
  withRollupLock(environmentId, (site) => Effect.gen(function* () {
    const rolledUpTo = site.rolledUpTo;
    const day = rolledUpTo === null ? undefined : site.dirty.filter((dirty) => dirty <= rolledUpTo).sort()[0];
    if (day === undefined) return false;
    yield* writeDay(site, day);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      update analytics.rollup_state set dirty = array_remove(dirty, ${day}::date), updated_at = now()
      where environment_id = ${environmentId}::text`);
    return true;
  }), false);

const repeat = <E, R>(step: Effect.Effect<boolean, E, R>) =>
  Effect.gen(function* () {
    let done = 0;
    while (done < MAX_DAYS_PER_SITE_RUN && (yield* step)) done++;
    return done;
  });

export const rollupSites = Effect.fn("WebAnalytics.rollupSites")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ environment_id: string }>(sql`
    select environment_id from analytics.site where enabled order by environment_id`, "objects");
  return rows.map((row) => row.environment_id);
});

export const closeDays = Effect.fn("WebAnalytics.closeDays")(function* (environmentId: string, now: Date) {
  return yield* repeat(closeNextDay(environmentId, now));
});

export const rebuildDirtyDays = Effect.fn("WebAnalytics.rebuildDirtyDays")(function* (environmentId: string) {
  return yield* repeat(rebuildNextDirty(environmentId));
});

export const ensureComingPartitions = Effect.fn("WebAnalytics.ensureComingPartitions")(function* (now: Date) {
  const months = [monthOf(now)];
  for (let n = 0; n < PARTITION_MONTHS_AHEAD; n++) months.push(nextMonth(months[months.length - 1] ?? monthOf(now)));
  yield* ensurePartitions(months);
});

export const expiredPartitions = Effect.fn("WebAnalytics.expiredPartitions")(function* (now: Date) {
  const cutoff = retentionCutoff(now.getTime());
  return (yield* pageviewPartitions()).filter((month) => new Date(`${nextMonth(month)}-01T00:00:00Z`) <= cutoff);
});

export const deleteExpiredAggregates = Effect.fn("WebAnalytics.deleteExpiredAggregates")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const cutoff = retentionCutoff(now.getTime());
  const cutoffDay = cutoff.toISOString().slice(0, 10);
  yield* drizzle.execute(sql`delete from analytics.daily where day < ${cutoffDay}::date`);
  yield* drizzle.execute(sql`delete from analytics.session_hourly where hour < ${instant(cutoff)}`);
  yield* drizzle.execute(sql`delete from analytics.bot_daily where day < ${cutoffDay}::date`);
  yield* drizzle.execute(sql`delete from analytics.gap_daily where day < ${cutoffDay}::date`);
});
