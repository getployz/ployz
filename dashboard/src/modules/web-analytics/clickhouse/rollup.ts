import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { addDays, dayEnd, dayStart, localDay, SESSION_GAP_SECONDS } from "../local-time";
import type { RollupSite } from "../postgres/rollup";
import { type Binder, binder, localHour, pageCounts, type Pin, RETAINED, requiredClickHouse, sessionCounts, visitCtes } from "./store";

type LockedSite = RollupSite & Pin & { readonly enabled: boolean; readonly rolledUpTo: string | null; readonly dirty: ReadonlyArray<string> };

const MAX_DAYS_PER_SITE_RUN = 31;

const withRollupLock = <A, E, R>(environmentId: string, program: (site: LockedSite) => Effect.Effect<A, E, R>, otherwise: A) =>
  Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`
        insert into analytics.clickhouse_site (environment_id)
        select environment_id from analytics.site where environment_id = ${environmentId}::text
        on conflict do nothing`);
      const [state] = yield* drizzle.execute<{ rolled_up_to: string | null; dirty: string[]; copy: string; floor: string; generation: string }>(sql`
        select rolled_up_to::text as rolled_up_to, dirty::text[] as dirty, copy::text as copy, floor::text as floor, generation::text as generation
        from analytics.clickhouse_site where environment_id = ${environmentId}::text for update`, "objects");
      const [site] = yield* drizzle.execute<{ timezone: string; enabled: boolean }>(sql`
        select timezone, enabled from analytics.site where environment_id = ${environmentId}::text`, "objects");
      if (state === undefined || site === undefined) return otherwise;
      return yield* program({
        environmentId,
        timezone: site.timezone,
        enabled: site.enabled,
        rolledUpTo: state.rolled_up_to,
        dirty: state.dirty,
        copy: Number(state.copy),
        floor: Number(state.floor),
        generation: Number(state.generation),
      });
    }));
  });

const PAGE_DIMS = `[('', ''), ('page', pathname), ('host', host), ('source', source), ('utm_source', utm_source),
  ('utm_medium', utm_medium), ('utm_campaign', utm_campaign), ('utm_term', utm_term), ('utm_content', utm_content),
  ('language', language)]`;
const SESSION_DIMS = `[('', ''), ('host', host), ('source', source), ('utm_source', utm_source),
  ('utm_medium', utm_medium), ('utm_campaign', utm_campaign), ('utm_term', utm_term), ('utm_content', utm_content),
  ('language', language), ('entry_page', pathname)]`;

const AGGREGATES = ["daily", "session_hourly"];

const dropUnpublished = Effect.fn("WebAnalytics.clickHouseDropUnpublished")(function* (site: LockedSite) {
  const clickhouse = yield* requiredClickHouse;
  const b = binder(site, site);
  const above = `environment_id = ${b.env} AND generation > ${b.generation}`;
  const [row] = yield* clickhouse.query<{ n: number }>(`
    SELECT (SELECT count() FROM daily WHERE ${above}) + (SELECT count() FROM session_hourly WHERE ${above}) AS n`, b.params);
  if (row === undefined || row.n === 0) return;
  for (const table of AGGREGATES) yield* clickhouse.command(`DELETE FROM ${table} WHERE ${above}`, b.params);
});

const writeUnpublishedDay = Effect.fn("WebAnalytics.clickHouseWriteUnpublishedDay")(function* (site: LockedSite, day: string) {
  const clickhouse = yield* requiredClickHouse;
  const start = dayStart(site.timezone, day);
  const end = dayEnd(site.timezone, day);
  const query = { environmentId: site.environmentId, timezone: site.timezone, range: { start, end }, filters: [] };
  const statement = (build: (b: Binder) => string) => {
    const b = binder(site, site);
    return clickhouse.command(build(b), b.params);
  };
  const ctes = (b: Binder) => `WITH ${visitCtes(b, query, { start, end })},
    sessions AS (SELECT * FROM visits WHERE ts >= ${b.at(start)} AND ts < ${b.at(end)})`;
  yield* dropUnpublished(site);
  const generation = site.generation + 1;
  yield* statement((b) => `
    INSERT INTO session_hourly (environment_id, day, generation, hour, pageviews, visitors, visits, bounces, total_duration)
    ${ctes(b)}
    SELECT ${b.env}, toDate(${b.text(day)}), ${generation}, toDateTime(intDiv(bucket, 1000), 'UTC'), sum(pv), sum(vi), sum(vs), sum(bo), sum(td)
    FROM (
      SELECT ${localHour(b)} AS bucket, ${pageCounts} FROM visit_rows GROUP BY bucket
      UNION ALL
      SELECT ${localHour(b)} AS bucket, ${sessionCounts} FROM sessions GROUP BY bucket
    ) GROUP BY bucket`);
  yield* statement((b) => `
    INSERT INTO daily (environment_id, day, generation, dim, value, pageviews, visitors, visits, bounces, total_duration)
    ${ctes(b)}
    SELECT ${b.env}, toDate(${b.text(day)}), ${generation}, dim, value, sum(pv), if(dim = 'entry_page', sum(svi), sum(pvi)), sum(vs), sum(bo), sum(td)
    FROM (
      SELECT dim, value, sum(n) AS pv, count() AS pvi, toUInt64(0) AS svi, toUInt64(0) AS vs, toUInt64(0) AS bo, toUInt64(0) AS td
      FROM (
        SELECT kv.1 AS dim, assumeNotNull(kv.2) AS value, visitor_id, count() AS n
        FROM (SELECT visitor_id, arrayJoin(${PAGE_DIMS}) AS kv FROM visit_rows)
        WHERE kv.2 IS NOT NULL GROUP BY dim, value, visitor_id
      ) GROUP BY dim, value
      UNION ALL
      SELECT dim, value, toUInt64(0) AS pv, toUInt64(0) AS pvi, count() AS svi, sum(n) AS vs, sum(b) AS bo, sum(d) AS td
      FROM (
        SELECT kv.1 AS dim, assumeNotNull(kv.2) AS value, visitor_id, count() AS n, countIf(pageviews = 1) AS b, sum(duration) AS d
        FROM (SELECT visitor_id, pageviews, duration, arrayJoin(${SESSION_DIMS}) AS kv FROM sessions)
        WHERE kv.2 IS NOT NULL GROUP BY dim, value, visitor_id
      ) GROUP BY dim, value
    ) GROUP BY dim, value`);
  return generation;
});

const firstDay = Effect.fn("WebAnalytics.clickHouseFirstDay")(function* (site: LockedSite) {
  const clickhouse = yield* requiredClickHouse;
  const b = binder(site, site);
  const [row] = yield* clickhouse.query<{ n: number; first: number }>(`
    SELECT count() AS n, toUnixTimestamp64Milli(min(ts)) AS first FROM pageview WHERE environment_id = ${b.env} AND copy = ${b.copy} AND ${RETAINED}`, b.params);
  return row === undefined || row.n === 0 ? null : localDay(site.timezone, row.first);
});

const closeNextDay = (environmentId: string, now: Date) =>
  withRollupLock(environmentId, (site) => Effect.gen(function* () {
    if (!site.enabled) return false;
    const day = site.rolledUpTo === null ? yield* firstDay(site) : addDays(site.timezone, site.rolledUpTo, 1);
    if (day === null || dayEnd(site.timezone, day).getTime() + SESSION_GAP_SECONDS * 1000 > now.getTime()) return false;
    const generation = yield* writeUnpublishedDay(site, day);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      update analytics.clickhouse_site
      set rolled_up_to = ${day}::date, dirty = array_remove(dirty, ${day}::date), generation = ${generation}, updated_at = now()
      where environment_id = ${environmentId}::text`);
    return true;
  }), false);

const rebuildNextDirty = (environmentId: string) =>
  withRollupLock(environmentId, (site) => Effect.gen(function* () {
    const rolledUpTo = site.rolledUpTo;
    const day = rolledUpTo === null ? undefined : site.dirty.filter((dirty) => dirty <= rolledUpTo).sort()[0];
    if (day === undefined) return false;
    const generation = yield* writeUnpublishedDay(site, day);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      update analytics.clickhouse_site set dirty = array_remove(dirty, ${day}::date), generation = ${generation}, updated_at = now()
      where environment_id = ${environmentId}::text`);
    return true;
  }), false);

const repeat = <E, R>(step: Effect.Effect<boolean, E, R>) =>
  Effect.gen(function* () {
    let done = 0;
    while (done < MAX_DAYS_PER_SITE_RUN && (yield* step)) done++;
    return done;
  });

export const closeDays = Effect.fn("WebAnalytics.clickHouseCloseDays")(function* (environmentId: string, now: Date) {
  return yield* repeat(closeNextDay(environmentId, now));
});

export const rebuildDirtyDays = Effect.fn("WebAnalytics.clickHouseRebuildDirtyDays")(function* (environmentId: string) {
  return yield* repeat(rebuildNextDirty(environmentId));
});
