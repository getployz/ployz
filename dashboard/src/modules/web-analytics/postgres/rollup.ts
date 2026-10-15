import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { addDays, dayEnd, dayStart, localDay, retentionFloor, SESSION_GAP_SECONDS } from "../local-time";
import { LONGEST_RETENTION_MONTHS } from "../model";
import { ensurePartitions, monthOf, nextMonth, partitionName } from "./partitions";
import { instant, localHour, sessionsIn, type StoreQuery } from "./store";

export type RollupSite = { readonly environmentId: string; readonly timezone: string };

const MAX_DAYS_PER_LOCK_HOLD = 31;
const PARTITION_MONTHS_AHEAD = 2;

const siteQuery = (site: RollupSite, start: Date, end: Date): StoreQuery =>
  ({ environmentId: site.environmentId, timezone: site.timezone, range: { start, end }, filters: [] });

const withRollupLock = <A, E, R>(environmentId: string, program: Effect.Effect<A, E, R>) =>
  Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`
        insert into analytics.rollup_state (environment_id) values (${environmentId}::text) on conflict do nothing`);
      yield* drizzle.execute(sql`
        select rolled_up_to from analytics.rollup_state where environment_id = ${environmentId}::text for update`);
      // A day's sessionization sorts and hashes a few hundred thousand rows; the default 4 MB spills every step to disk.
      yield* drizzle.execute(sql`set local work_mem = '64MB'`);
      return yield* program;
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
    with sessions as materialized (${sessionsIn(query, { start, end })}),
    hourly as (
      insert into analytics.session_hourly (environment_id, hour, visits, bounces, total_duration)
      select ${site.environmentId}::text, ${localHour(site.timezone)} as hour, count(*)::bigint,
        count(*) filter (where pageviews = 1)::bigint, coalesce(sum(duration), 0)::bigint
      from sessions
      group by 2
    ), pv as (
      select dim, value, sum(pageviews)::bigint as pageviews, count(*)::bigint as visitors
      from (
        select kv.dim, kv.value, visitor_id, count(*) as pageviews
        from analytics.pageview
        cross join lateral (values ${PAGE_DIMS}) as kv(dim, value)
        where environment_id = ${site.environmentId}::text and ts >= ${instant(start)} and ts < ${instant(end)}
          and kv.value is not null
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

export const rebuildDay = Effect.fn("WebAnalytics.rebuildDay")(function* (site: RollupSite, day: string) {
  yield* withRollupLock(site.environmentId, Effect.gen(function* () {
    yield* writeDay(site, day);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      update analytics.rollup_state set dirty = array_remove(dirty, ${day}::date), updated_at = now()
      where environment_id = ${site.environmentId}::text`);
  }));
});

const closeDay = (site: RollupSite, day: string) =>
  withRollupLock(site.environmentId, Effect.gen(function* () {
    yield* writeDay(site, day);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      update analytics.rollup_state
      set rolled_up_to = ${day}::date, dirty = array_remove(dirty, ${day}::date), updated_at = now()
      where environment_id = ${site.environmentId}::text`);
  }));

type SiteState = RollupSite & { readonly rolledUpTo: string | null; readonly dirty: ReadonlyArray<string> };

const enabledSites = Effect.fn("WebAnalytics.enabledSites")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ environment_id: string; timezone: string; rolled_up_to: string | null; dirty: string }>(sql`
    select s.environment_id, s.timezone, r.rolled_up_to::text as rolled_up_to,
      coalesce(array_to_string(r.dirty, ','), '') as dirty
    from analytics.site s left join analytics.rollup_state r using (environment_id)
    where s.enabled
    order by s.environment_id`, "objects");
  return rows.map((row): SiteState => ({
    environmentId: row.environment_id,
    timezone: row.timezone,
    rolledUpTo: row.rolled_up_to,
    dirty: row.dirty === "" ? [] : row.dirty.split(","),
  }));
});

const firstDay = Effect.fn("WebAnalytics.firstDay")(function* (site: RollupSite) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ ts: string | null }>(sql`
    select min(ts)::text as ts from analytics.pageview where environment_id = ${site.environmentId}::text`, "objects");
  const ts = rows[0]?.ts;
  return ts == null ? null : localDay(site.timezone, new Date(ts));
});

export const rollupClosedDays = Effect.fn("WebAnalytics.rollupClosedDays")(function* (now: Date) {
  let closed = 0;
  for (const site of yield* enabledSites()) {
    let day = site.rolledUpTo === null ? yield* firstDay(site) : addDays(site.timezone, site.rolledUpTo, 1);
    for (let n = 0; day !== null && n < MAX_DAYS_PER_LOCK_HOLD; n++) {
      if (dayEnd(site.timezone, day).getTime() + SESSION_GAP_SECONDS * 1000 > now.getTime()) break;
      yield* closeDay(site, day);
      closed++;
      day = addDays(site.timezone, day, 1);
    }
  }
  return closed;
});

export const rebuildDirtyDays = Effect.fn("WebAnalytics.rebuildDirtyDays")(function* () {
  let rebuilt = 0;
  for (const site of yield* enabledSites()) {
    for (const day of site.dirty) {
      yield* rebuildDay(site, day);
      rebuilt++;
    }
  }
  return rebuilt;
});

const PARTITION_PATTERN = /^pageview_(\d{4})(\d{2})$/;

export const applyRetention = Effect.fn("WebAnalytics.applyRetention")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const months = [monthOf(now)];
  for (let n = 0; n < PARTITION_MONTHS_AHEAD; n++) months.push(nextMonth(months[months.length - 1] ?? monthOf(now)));
  yield* ensurePartitions(months);

  const oldest = new Date(now);
  oldest.setUTCMonth(oldest.getUTCMonth() - LONGEST_RETENTION_MONTHS);
  const children = yield* drizzle.execute<{ name: string }>(sql`
    select c.relname as name
    from pg_inherits i
    join pg_class c on c.oid = i.inhrelid
    join pg_class p on p.oid = i.inhparent
    join pg_namespace n on n.oid = p.relnamespace
    where n.nspname = 'analytics' and p.relname = 'pageview'`, "objects");
  const dropped: string[] = [];
  for (const { name } of children) {
    const match = PARTITION_PATTERN.exec(name);
    if (match === null) continue;
    const month = `${match[1]}-${match[2]}`;
    if (new Date(`${nextMonth(month)}-01T00:00:00Z`) > oldest) continue;
    yield* drizzle.execute(sql.raw(`drop table if exists analytics.${partitionName(month)}`));
    dropped.push(name);
  }

  for (const site of yield* enabledSites()) {
    const floor = retentionFloor(site.timezone, now.getTime(), LONGEST_RETENTION_MONTHS);
    const floorDay = localDay(site.timezone, floor);
    yield* drizzle.execute(sql`
      delete from analytics.daily where environment_id = ${site.environmentId}::text and day < ${floorDay}::date`);
    yield* drizzle.execute(sql`
      delete from analytics.session_hourly where environment_id = ${site.environmentId}::text and hour < ${instant(floor)}`);
    yield* drizzle.execute(sql`
      delete from analytics.bot_daily where environment_id = ${site.environmentId}::text and day < ${floorDay}::date`);
    yield* drizzle.execute(sql`
      delete from analytics.gap_daily where environment_id = ${site.environmentId}::text and day < ${floorDay}::date`);
  }
  return dropped;
});
