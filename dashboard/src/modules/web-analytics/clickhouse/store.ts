import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { dayEnd, dayStart, ENTRY_LOOKBACK_SECONDS, localDay, SESSION_GAP_SECONDS } from "../local-time";
import { type BucketRow, type EntryRow, type Filter, type FilterKey, type GoalRow, type Range, SESSION_DIMS, type SessionDim, type SessionRow, type ValueRow } from "../model";
import { COLUMN, pageFilters, type Plan, readPlan, SESSION_KEYS, sessionFilters, type StoreQuery, totalsKey } from "../postgres/store";
import { ClickHouse } from "./client";

type Params = Record<string, string | number | ReadonlyArray<string>>;

export type Binder = {
  readonly params: Params;
  readonly env: string;
  readonly tz: string;
  readonly text: (value: string) => string;
  readonly texts: (values: ReadonlyArray<string>) => string;
  readonly at: (instant: Date) => string;
};

export function binder(query: Pick<StoreQuery, "environmentId" | "timezone">): Binder {
  const params: Params = {};
  let next = 0;
  const bind = (type: string, value: string | number | ReadonlyArray<string>) => {
    const name = `p${next++}`;
    params[name] = value;
    return `{${name}:${type}}`;
  };
  const text = (value: string) => bind("String", value);
  return {
    params,
    env: text(query.environmentId),
    tz: text(query.timezone),
    text,
    texts: (values) => bind("Array(String)", values),
    at: (instant) => `fromUnixTimestamp64Milli(${bind("Int64", instant.getTime())}, 'UTC')`,
  };
}

export const RETAINED = "ts >= now() - toIntervalDay(retention_days)";
const PAGEVIEW_COLUMNS = "ts, visitor_id, host, pathname, source, utm_source, utm_medium, utm_campaign, utm_term, utm_content, language";
const ORDER = "ORDER BY ts, pathname, host";
const plus = (instant: Date, seconds: number) => new Date(instant.getTime() + seconds * 1000);

const pageviewsIn = (b: Binder, start: Date, end: Date) =>
  `environment_id = ${b.env} AND ts >= ${b.at(start)} AND ts < ${b.at(end)} AND ${RETAINED}`;

const localDate = (b: Binder) => `toString(toDate(ts, ${b.tz}))`;

export const localHour = (b: Binder) => {
  const local = `toTimeZone(ts, ${b.tz})`;
  const back = `(ts - toIntervalMillisecond((toMinute(${local}) * 60 + toSecond(${local})) * 1000 + toMillisecond(${local})))`;
  const offset = (at: string) => `timeZoneOffset(toTimeZone(${at}, ${b.tz}))`;
  const search = `arrayMin(arrayFilter(m -> ${offset("m")} = ${offset("ts")}, arrayMap(i -> ${back} + toIntervalMinute(i), range(1, 61))))`;
  return `toUnixTimestamp64Milli(if(${offset(back)} = ${offset("ts")}, ${back}, ${search}))`;
};

const filterClause = (b: Binder, filters: ReadonlyArray<Filter>) =>
  filters.map((filter) => {
    const target = COLUMN[filter.key];
    const value = b.text(filter.value);
    return filter.op === "eq" ? ` AND ${target} = ${value}` : ` AND (${target} IS NULL OR ${target} != ${value})`;
  }).join("");

const HEAD_DIMS = SESSION_DIMS.map((dim) => `first_value(${dim}) RESPECT NULLS OVER visit AS head_${dim}`).join(", ");
const HEAD_ALIASES = SESSION_DIMS.map((dim) => `head_${dim} AS ${dim}`).join(", ");

/**
 * Head dims take `head_` names in `attributed` because a ClickHouse alias shadows the column of the same name
 * everywhere in its query, the visit window's ordering included.
 */
export const visitCtes = (b: Binder, query: StoreQuery, range: Range) => {
  const picks = pageFilters(query);
  const selected = `${filterClause(b, sessionFilters(query))}${picks.length > 0 ? " AND picked > 0" : ""}`;
  const gapStart = plus(range.start, -SESSION_GAP_SECONDS);
  return `
  window_rows AS (
    SELECT ${PAGEVIEW_COLUMNS} FROM pageview
    WHERE ${pageviewsIn(b, gapStart, plus(range.end, 86_400))}
    UNION ALL
    SELECT ${PAGEVIEW_COLUMNS} FROM pageview
    WHERE ${pageviewsIn(b, plus(range.start, -ENTRY_LOOKBACK_SECONDS), gapStart)}
      AND visitor_id IN (SELECT visitor_id FROM pageview WHERE ${pageviewsIn(b, gapStart, range.start)})
  ), marked AS (
    SELECT *, if(row_number() OVER w > 1 AND lagInFrame(ts) OVER w + toIntervalSecond(${SESSION_GAP_SECONDS}) >= ts, 0, 1) AS is_new
    FROM window_rows WINDOW w AS (PARTITION BY visitor_id ${ORDER} ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
  ), numbered AS (
    SELECT *, sum(is_new) OVER (PARTITION BY visitor_id ${ORDER} ROWS UNBOUNDED PRECEDING) AS sn FROM marked
  ), attributed AS (
    SELECT ts, visitor_id, pathname, is_new, ${HEAD_DIMS}, max(ts) OVER visit AS ended, count() OVER visit AS pageviews,
      countIf(true${filterClause(b, picks)}) OVER visit AS picked
    FROM numbered
    WINDOW visit AS (PARTITION BY visitor_id, sn ${ORDER} ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
  ), visits AS (
    SELECT ts, visitor_id, pathname, ${HEAD_ALIASES}, pageviews, picked,
      toUInt64(intDiv(toUnixTimestamp64Milli(ended) - toUnixTimestamp64Milli(ts), 1000)) AS duration
    FROM attributed WHERE is_new = 1${selected}
  ), visit_rows AS (
    SELECT ts, visitor_id, pathname, ${HEAD_ALIASES}, picked
    FROM attributed
    WHERE ts >= ${b.at(range.start)} AND ts < ${b.at(range.end)}${selected}${filterClause(b, picks)}
  )`;
};

const sessionsIn = (b: Binder, query: StoreQuery, range: Range) => `
  WITH ${visitCtes(b, query, range)}
  SELECT * FROM visits WHERE ts >= ${b.at(range.start)} AND ts < ${b.at(range.end)}`;

const rowsIn = (b: Binder, query: StoreQuery, range: Range, needsVisit = false) =>
  query.filters.length === 0 && !needsVisit
    ? `FROM pageview WHERE ${pageviewsIn(b, range.start, range.end)}`
    : `FROM (WITH ${visitCtes(b, query, range)} SELECT * FROM visit_rows) WHERE true`;

const rollupMarker = Effect.fn("WebAnalytics.clickHouseRollupMarker")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ day: string | null; dirty: string[] }>(sql`
    select rolled_up_to::text as day, dirty::text[] as dirty from analytics.clickhouse_site where environment_id = ${environmentId}::text`, "objects");
  return { rolledUpTo: rows[0]?.day ?? null, dirty: rows[0]?.dirty ?? [] };
});

const planned = Effect.fn("WebAnalytics.clickHousePlan")(function* (query: StoreQuery, rolled?: boolean) {
  return readPlan(query, (yield* rollupMarker(query.environmentId)).rolledUpTo, rolled);
});

export const requiredClickHouse = Effect.gen(function* () {
  const clickhouse = yield* ClickHouse;
  if (clickhouse === null) return yield* Effect.die("The ClickHouse store ran without a ClickHouse client.");
  return clickhouse;
});

const run = Effect.fn("WebAnalytics.clickHouseRead")(function* <A>(query: StoreQuery, build: (b: Binder) => string) {
  const clickhouse = yield* requiredClickHouse;
  const b = binder(query);
  return yield* clickhouse.query<A>(build(b), b.params);
});

const published = (b: Binder, first: string, last: string) => `(day, generation) IN (
  SELECT day, max(generation) FROM daily
  WHERE environment_id = ${b.env} AND day BETWEEN toDate(${b.text(first)}) AND toDate(${b.text(last)}) GROUP BY day)`;

const dailyRows = (b: Binder, days: NonNullable<Plan["days"]>, dim: string, value?: string) => `
  FROM daily
  WHERE environment_id = ${b.env} AND dim = ${b.text(dim)}${value === undefined ? "" : ` AND value = ${b.text(value)}`}
    AND day BETWEEN toDate(${b.text(days.first)}) AND toDate(${b.text(days.last)}) AND ${published(b, days.first, days.last)}`;

const ZERO = "toUInt64(0)";

/** Parts name their counts `pv vi vs bo td`, never a column name: an alias would shadow the column it counts. */
const unionTotals = (key: string, parts: ReadonlyArray<string>) => `
  SELECT ${key}, sum(pv) AS pageviews, sum(vi) AS visitors, sum(vs) AS visits, sum(bo) AS bounces, sum(td) AS total_duration
  FROM (${parts.join(" UNION ALL ")}) GROUP BY ${key}`;

export const pageCounts = `count() AS pv, uniqExact(visitor_id) AS vi, ${ZERO} AS vs, ${ZERO} AS bo, ${ZERO} AS td`;
export const sessionCounts = `${ZERO} AS pv, ${ZERO} AS vi, count() AS vs, countIf(pageviews = 1) AS bo, sum(duration) AS td`;

type Totals = { pageviews: number; visitors: number; visits: number; bounces: number; total_duration: number };
const totalsOf = (row: Totals) => ({
  pageviews: row.pageviews,
  visitors: row.visitors,
  visits: row.visits,
  bounces: row.bounces,
  totalDuration: row.total_duration,
});

export const dailyTotals = Effect.fn("WebAnalytics.clickHouseDailyTotals")(function* (query: StoreQuery) {
  const key = totalsKey(query.filters);
  const plan = yield* planned(query, key !== null);
  const rows = yield* run<Totals & { local_day: string }>(query, (b) => {
    const parts: string[] = [];
    if (plan.days !== null && key !== null) {
      parts.push(`SELECT toString(day) AS local_day, pageviews AS pv, visitors AS vi, visits AS vs, bounces AS bo, total_duration AS td
        ${dailyRows(b, plan.days, key.dim, key.value)}`);
    }
    for (const edge of plan.edges) {
      parts.push(`SELECT ${localDate(b)} AS local_day, ${pageCounts} ${rowsIn(b, query, edge)} GROUP BY local_day`);
      parts.push(`SELECT ${localDate(b)} AS local_day, ${sessionCounts} FROM (${sessionsIn(b, query, edge)}) GROUP BY local_day`);
    }
    return unionTotals("local_day", parts);
  });
  return rows.map((row): BucketRow => ({ bucket: dayStart(query.timezone, row.local_day), ...totalsOf(row) }));
});

const staleVisitHours = (timezone: string, dirty: ReadonlyArray<string>, range: Range): Range[] =>
  dirty
    .map((day) => ({ start: new Date(Math.max(dayStart(timezone, day).getTime(), range.start.getTime())), end: new Date(Math.min(dayEnd(timezone, day).getTime(), range.end.getTime())) }))
    .filter((part) => part.start < part.end);

export const hourlyTotals = Effect.fn("WebAnalytics.clickHouseHourlyTotals")(function* (query: StoreQuery) {
  const { timezone, range } = query;
  if (range.start >= range.end) return [];
  const marker = query.filters.length > 0 ? null : yield* rollupMarker(query.environmentId);
  const closedUntil = marker?.rolledUpTo ?? null;
  const rolledEnd = closedUntil === null ? range.start : new Date(Math.min(Math.max(dayEnd(timezone, closedUntil).getTime(), range.start.getTime()), range.end.getTime()));
  const rows = yield* run<Totals & { bucket: number }>(query, (b) => {
    const parts: string[] = [];
    if (rolledEnd > range.start) {
      const dirty = staleVisitHours(timezone, marker?.dirty ?? [], { start: range.start, end: rolledEnd });
      const inDirty = dirty.length === 0 ? "false" : dirty.map((part) => `(hour >= ${b.at(part.start)} AND hour < ${b.at(part.end)})`).join(" OR ");
      parts.push(`
        SELECT toInt64(toUnixTimestamp(hour)) * 1000 AS bucket, if(${inDirty}, 0, pageviews) AS pv, if(${inDirty}, 0, visitors) AS vi,
          visits AS vs, bounces AS bo, total_duration AS td
        FROM session_hourly WHERE environment_id = ${b.env} AND hour >= ${b.at(range.start)} AND hour < ${b.at(rolledEnd)}
          AND ${published(b, localDay(timezone, range.start), localDay(timezone, rolledEnd.getTime() - 1))}`);
      for (const part of dirty) parts.push(`SELECT ${localHour(b)} AS bucket, ${pageCounts} ${rowsIn(b, query, part)} GROUP BY bucket`);
    }
    if (rolledEnd < range.end) {
      const edge = { start: rolledEnd, end: range.end };
      parts.push(`SELECT ${localHour(b)} AS bucket, ${pageCounts} ${rowsIn(b, query, edge)} GROUP BY bucket`);
      parts.push(`SELECT ${localHour(b)} AS bucket, ${sessionCounts} FROM (${sessionsIn(b, query, edge)}) GROUP BY bucket`);
    }
    return unionTotals("bucket", parts);
  });
  return rows.map((row): BucketRow => ({ bucket: new Date(row.bucket), ...totalsOf(row) }));
});

export const breakdown = Effect.fn("WebAnalytics.clickHouseBreakdown")(function* (query: StoreQuery, dim: FilterKey) {
  const plan = yield* planned(query);
  const column = COLUMN[dim];
  const rows = yield* run<{ value: string; pageviews: number; visitors: number }>(query, (b) => {
    const parts: string[] = [];
    if (plan.days !== null) parts.push(`SELECT value, pageviews AS pv, visitors AS vi ${dailyRows(b, plan.days, dim)}`);
    for (const edge of plan.edges) {
      parts.push(`SELECT ${column} AS value, count() AS pv, uniqExact(visitor_id) AS vi
        ${rowsIn(b, query, edge, SESSION_KEYS.has(dim))} AND ${column} IS NOT NULL GROUP BY value, ${localDate(b)}`);
    }
    return `SELECT value, sum(pv) AS pageviews, sum(vi) AS visitors FROM (${parts.join(" UNION ALL ")}) GROUP BY value`;
  });
  return rows.map((row): ValueRow => ({ value: row.value, pageviews: row.pageviews, visitors: row.visitors }));
});

export const sessionsBy = Effect.fn("WebAnalytics.clickHouseSessionsBy")(function* (query: StoreQuery, dim: SessionDim) {
  const plan = yield* planned(query);
  const rows = yield* run<Totals & { value: string }>(query, (b) => {
    const parts: string[] = [];
    if (plan.days !== null) {
      parts.push(`SELECT value, ${ZERO} AS pv, ${ZERO} AS vi, visits AS vs, bounces AS bo, total_duration AS td
        ${dailyRows(b, plan.days, dim)} AND visits > 0`);
    }
    for (const edge of plan.edges) {
      parts.push(`SELECT ${dim} AS value, ${sessionCounts} FROM (${sessionsIn(b, query, edge)}) WHERE ${dim} IS NOT NULL GROUP BY value`);
    }
    return unionTotals("value", parts);
  });
  return rows.map((row): SessionRow => ({ value: row.value, visits: row.visits, bounces: row.bounces, totalDuration: row.total_duration }));
});

export const entryPages = Effect.fn("WebAnalytics.clickHouseEntryPages")(function* (query: StoreQuery) {
  const plan = yield* planned(query);
  const rows = yield* run<Totals & { value: string }>(query, (b) => {
    const parts: string[] = [];
    if (plan.days !== null) {
      parts.push(`SELECT value, ${ZERO} AS pv, visitors AS vi, visits AS vs, bounces AS bo, total_duration AS td
        ${dailyRows(b, plan.days, "entry_page")}`);
    }
    for (const edge of plan.edges) {
      parts.push(`SELECT pathname AS value, ${ZERO} AS pv, uniqExact(visitor_id) AS vi, count() AS vs, countIf(pageviews = 1) AS bo,
          sum(duration) AS td
        FROM (${sessionsIn(b, query, edge)}) GROUP BY value, ${localDate(b)}`);
    }
    return unionTotals("value", parts);
  });
  return rows.map((row): EntryRow => ({
    value: row.value, visitors: row.visitors, visits: row.visits, bounces: row.bounces, totalDuration: row.total_duration,
  }));
});

export const goalConversions = Effect.fn("WebAnalytics.clickHouseGoalConversions")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const goals = yield* drizzle.execute<{ id: string; name: string; path_pattern: string }>(sql`
    select id, name, path_pattern from analytics.goal where environment_id = ${query.environmentId}::text`, "objects");
  if (goals.length === 0) return [];
  const rows = yield* run<{ goal: number; visitors: number; conversions: number }>(query, (b) => `
    WITH range_rows AS (SELECT pathname, ts, visitor_id ${rowsIn(b, query, query.range)}),
    goals AS (
      SELECT goal, replaceAll(replaceAll(replaceAll(replaceAll(path_pattern, '\\\\', '\\\\\\\\'), '%', '\\\\%'), '_', '\\\\_'), '*', '%') AS pattern
      FROM (SELECT arrayJoin(arrayEnumerate(patterns)) AS goal, patterns[goal] AS path_pattern FROM (SELECT ${b.texts(goals.map((goal) => goal.path_pattern))} AS patterns))
    ), matched AS (
      SELECT goals.goal AS goal, paths.pathname AS pathname
      FROM goals CROSS JOIN (SELECT DISTINCT pathname FROM range_rows) AS paths WHERE paths.pathname LIKE goals.pattern
    ), hits AS (
      SELECT matched.goal AS goal, toDate(range_rows.ts, ${b.tz}) AS day, range_rows.visitor_id AS visitor, count() AS hits
      FROM matched INNER JOIN range_rows ON range_rows.pathname = matched.pathname
      GROUP BY goal, day, visitor
    )
    SELECT goal, count() AS visitors, sum(hits) AS conversions FROM hits GROUP BY goal`);
  const byGoal = new Map(rows.map((row) => [row.goal, row]));
  return goals.map((goal, index): GoalRow => ({
    goalId: goal.id,
    name: goal.name,
    visitors: byGoal.get(index + 1)?.visitors ?? 0,
    conversions: byGoal.get(index + 1)?.conversions ?? 0,
  }));
});
