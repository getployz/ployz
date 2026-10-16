import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { addDays, dayEnd, dayStart, localDay, SESSION_GAP_SECONDS } from "../local-time";
import type { RollupSite } from "../postgres/rollup";
import { type Attempt, insertSelect, mint, type Published, publish, PUBLISHED_COLUMNS, publishedOf, type PublishedRow } from "./attempt";
import { type Binder, binder, localHour, pageCounts, RETAINED, requiredClickHouse, sessionCounts, visitCtes } from "./store";

type SiteState = RollupSite & { readonly enabled: boolean; readonly rolledUpTo: string | null; readonly dirty: ReadonlyArray<string>; readonly published: Published };

const MAX_DAYS_PER_SITE_RUN = 31;

const siteState = Effect.fn("WebAnalytics.clickHouseSiteState")(function* (environmentId: string, lock: boolean) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<PublishedRow & { rolled_up_to: string | null; dirty: string[] }>(sql`
    select rolled_up_to::text as rolled_up_to, dirty::text[] as dirty, ${PUBLISHED_COLUMNS}
    from analytics.clickhouse_site where environment_id = ${environmentId}::text ${lock ? sql`for update` : sql``}`, "objects");
  const [site] = yield* drizzle.execute<{ timezone: string; enabled: boolean }>(sql`
    select timezone, enabled from analytics.site where environment_id = ${environmentId}::text`, "objects");
  if (site === undefined) return null;
  return {
    environmentId,
    timezone: site.timezone,
    enabled: site.enabled,
    rolledUpTo: row?.rolled_up_to ?? null,
    dirty: row?.dirty ?? [],
    published: publishedOf(row),
  } satisfies SiteState;
});

const PAGE_DIMS = `[('', ''), ('page', pathname), ('host', host), ('source', source), ('utm_source', utm_source),
  ('utm_medium', utm_medium), ('utm_campaign', utm_campaign), ('utm_term', utm_term), ('utm_content', utm_content),
  ('language', language)]`;
const SESSION_DIMS = `[('', ''), ('host', host), ('source', source), ('utm_source', utm_source),
  ('utm_medium', utm_medium), ('utm_campaign', utm_campaign), ('utm_term', utm_term), ('utm_content', utm_content),
  ('language', language), ('entry_page', pathname)]`;

const writeDay = Effect.fn("WebAnalytics.clickHouseWriteDay")(function* (site: SiteState, day: string, attempt: Attempt) {
  const clickhouse = yield* requiredClickHouse;
  const start = dayStart(site.timezone, day);
  const end = dayEnd(site.timezone, day);
  const query = { environmentId: site.environmentId, timezone: site.timezone, range: { start, end }, filters: [] };
  const statement = (build: (b: Binder) => string) => {
    const b = binder(site, site.published);
    return { query: build(b), params: b.params };
  };
  const ctes = (b: Binder) => `WITH ${visitCtes(b, query, { start, end })},
    sessions AS (SELECT * FROM visits WHERE ts >= ${b.at(start)} AND ts < ${b.at(end)})`;
  return yield* insertSelect(clickhouse, attempt, [
    statement((b) => `
    INSERT INTO session_hourly (environment_id, day, attempt, hour, pageviews, visitors, visits, bounces, total_duration)
    ${ctes(b)}
    SELECT ${b.env}, toDate(${b.text(day)}), {attempt:UInt64}, toDateTime(intDiv(bucket, 1000), 'UTC'), sum(pv), sum(vi), sum(vs), sum(bo), sum(td)
    FROM (
      SELECT ${localHour(b)} AS bucket, ${pageCounts} FROM visit_rows GROUP BY bucket
      UNION ALL
      SELECT ${localHour(b)} AS bucket, ${sessionCounts} FROM sessions GROUP BY bucket
    ) GROUP BY bucket`),
    statement((b) => `
    INSERT INTO daily (environment_id, day, attempt, dim, value, pageviews, visitors, visits, bounces, total_duration)
    ${ctes(b)}
    SELECT ${b.env}, toDate(${b.text(day)}), {attempt:UInt64}, dim, value, sum(pv), if(dim = 'entry_page', sum(svi), sum(pvi)), sum(vs), sum(bo), sum(td)
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
    ) GROUP BY dim, value`),
  ]);
});

const firstDay = Effect.fn("WebAnalytics.clickHouseFirstDay")(function* (site: SiteState) {
  const clickhouse = yield* requiredClickHouse;
  const b = binder(site, site.published);
  const [row] = yield* clickhouse.query<{ n: number; first: number }>(`
    SELECT count() AS n, toUnixTimestamp64Milli(min(ts)) AS first FROM pageview WHERE environment_id = ${b.env} AND ${b.visible} AND ${RETAINED}`, b.params);
  return row === undefined || row.n === 0 ? null : localDay(site.timezone, row.first);
});

/**
 * Writes one day under a fresh attempt and publishes it. The site row stays locked from the pick through the publish, so the
 * pageview set the day was computed from is the set that is visible when it lands. The peek before minting keeps an idle
 * site from accumulating attempts that never publish.
 */
const rollupDay = <E, R>(environmentId: string, pick: (site: SiteState) => Effect.Effect<string | null, E, R>, set: (day: string, attempt: Attempt) => SQL) =>
  Effect.gen(function* () {
    const peeked = yield* siteState(environmentId, false);
    if (peeked === null || (yield* pick(peeked)) === null) return false;
    const attempt = (yield* mint([environmentId])).get(environmentId);
    if (attempt === undefined) return false;
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const site = yield* siteState(environmentId, true);
      if (site === null) return false;
      const day = yield* pick(site);
      if (day === null) return false;
      const proof = yield* writeDay(site, day, attempt);
      yield* publish(proof, environmentId, set(day, attempt));
      return true;
    }));
  });

const closeNextDay = (environmentId: string, now: Date) =>
  rollupDay(environmentId, (site) => Effect.gen(function* () {
    if (!site.enabled) return null;
    const day = site.rolledUpTo === null ? yield* firstDay(site) : addDays(site.timezone, site.rolledUpTo, 1);
    return day === null || dayEnd(site.timezone, day).getTime() + SESSION_GAP_SECONDS * 1000 > now.getTime() ? null : day;
  }), (day, attempt) => sql`
    rolled_up_to = ${day}::date, dirty = array_remove(dirty, ${day}::date), days = days || jsonb_build_object(${day}::text, ${attempt}::bigint)`);

const rebuildNextDirty = (environmentId: string) =>
  rollupDay(environmentId, (site) => {
    const closed = site.rolledUpTo;
    return Effect.succeed(closed === null ? null : site.dirty.filter((dirty) => dirty <= closed).sort()[0] ?? null);
  }, (day, attempt) => sql`dirty = array_remove(dirty, ${day}::date), days = days || jsonb_build_object(${day}::text, ${attempt}::bigint)`);

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
