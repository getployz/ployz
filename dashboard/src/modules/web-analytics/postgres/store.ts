import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { dayEnd, dayStart, localDays, SESSION_GAP_SECONDS } from "../local-time";
import { type BucketRow, type EntryRow, type Filter, type FilterKey, type GoalRow, type Range, SESSION_DIMS, type SessionDim, type SessionRow, type ValueRow } from "../model";

export type StoreQuery = {
  readonly environmentId: string;
  readonly timezone: string;
  readonly range: Range;
  readonly filters: ReadonlyArray<Filter>;
};

const COLUMN = {
  page: "pathname",
  host: "host",
  source: "source",
  utm_source: "utm_source",
  utm_medium: "utm_medium",
  utm_campaign: "utm_campaign",
  utm_term: "utm_term",
  utm_content: "utm_content",
  language: "language",
} satisfies Record<FilterKey, string>;

const column = (key: FilterKey) => sql.raw(COLUMN[key]);
export const instant = (date: Date) => sql`${date.toISOString()}::timestamptz`;
export const localDate = (timezone: string) => sql`(ts at time zone ${timezone}::text)::date`;
/**
 * The instant at which the site-local hour holding `ts` starts, as {@link hourStart} computes it. Subtracting the
 * wall-clock seconds past the hour stays exact through a repeated DST hour, where truncating a wall time would pick one
 * of its two instants. When an offset change falls inside the hour that lands before it, so the hour starts at the
 * change, found by the minute; only rows in such an hour run the search.
 */
export const localHour = (timezone: string) => {
  const offset = (at: SQL) => sql`((${at}) at time zone ${timezone}::text) - ((${at}) at time zone 'UTC')`;
  const back = sql`ts - make_interval(secs => mod(extract(epoch from (ts at time zone ${timezone}::text)), 3600)::double precision)`;
  return sql`case when ${offset(back)} = ${offset(sql`ts`)} then ${back} else (
    select min(m) from generate_series(${back} + interval '1 minute', ts, interval '1 minute') m
    where ${offset(sql`m`)} = ${offset(sql`ts`)}) end`;
};
const gap = sql.raw(`interval '${SESSION_GAP_SECONDS} seconds'`);
const epochMs = (expr: SQL) => sql`(extract(epoch from ${expr}) * 1000)::bigint`;

const SESSION_KEYS: ReadonlySet<FilterKey> = new Set(SESSION_DIMS);
const sessionFilters = (query: StoreQuery) => query.filters.filter((filter) => SESSION_KEYS.has(filter.key));
const pageFilters = (query: StoreQuery) => query.filters.filter((filter) => !SESSION_KEYS.has(filter.key));

const filterClause = (filters: ReadonlyArray<Filter>, table?: string) =>
  sql.join(
    filters.map((filter) => {
      const target = sql.raw(table === undefined ? COLUMN[filter.key] : `${table}.${COLUMN[filter.key]}`);
      return filter.op === "eq"
        ? sql` and ${target} = ${filter.value}::text`
        : sql` and ${target} is distinct from ${filter.value}::text`;
    }),
    sql``,
  );

const HEAD_DIMS = sql.raw(SESSION_DIMS.map((dim) => `first_value(${dim}) over visit as ${dim}`).join(", "));

/**
 * Sessionizes every pageview in the range's window, filtered or not, and keeps as `visits` the sessions the filters
 * select: session dim filters test the first pageview, page filters need at least one pageview of the session to pass.
 * The window reaches a session gap before the range for exact session starts, and a day past it for whole sessions.
 * Each pageview takes its visit's first values and counts from window functions, never a join: the planner has no row
 * estimates for a CTE, and with a filter it picked nested loops that went quadratic.
 */
export const visitCtes = (query: StoreQuery, range: Range) => {
  const picks = pageFilters(query);
  const selected = sql`${filterClause(sessionFilters(query))}${picks.length > 0 ? sql` and picked > 0` : sql``}`;
  return sql`
  window_rows as (
    select ts, visitor_id, host, pathname, source, utm_source, utm_medium, utm_campaign, utm_term, utm_content, language
    from analytics.pageview
    where environment_id = ${query.environmentId}::text
      and ts >= ${instant(range.start)} - ${gap}
      and ts < ${instant(range.end)} + interval '1 day'
  ), marked as (
    select *, case when ts - lag(ts) over w <= ${gap} then 0 else 1 end as is_new
    from window_rows window w as (partition by visitor_id order by ts, pathname, host)
  ), numbered as (
    select *, sum(is_new) over (partition by visitor_id order by ts, pathname, host rows unbounded preceding) as sn
    from marked
  ), attributed as (
    select ts, visitor_id, pathname, is_new, ${HEAD_DIMS}, max(ts) over visit as ended, count(*) over visit as pageviews,
      count(*) filter (where true${filterClause(picks)}) over visit as picked
    from numbered
    window visit as (partition by visitor_id, sn order by ts, pathname, host rows between unbounded preceding and unbounded following)
  ), visits as (
    select *, floor(extract(epoch from (ended - ts)))::bigint as duration
    from attributed where is_new = 1${selected}
  ), visit_rows as (
    select ts, visitor_id, pathname, ${sql.raw(SESSION_DIMS.join(", "))}
    from attributed
    where ts >= ${instant(range.start)} and ts < ${instant(range.end)}${selected}${filterClause(picks)}
  )`;
};

/** The selected visits that start in the range, with their first pageview's columns. */
export const sessionsIn = (query: StoreQuery, range: Range) => sql`
  with ${visitCtes(query, range)}
  select * from visits where ts >= ${instant(range.start)} and ts < ${instant(range.end)}`;

/**
 * The selected pageviews in the range, each carrying its visit's session dims, as a `from ... where ...` clause.
 * Without filters a pageview's own page and the visit counts need no sessionizing, so those reads skip it.
 */
const rowsIn = (query: StoreQuery, range: Range, needsVisit = false) =>
  query.filters.length === 0 && !needsVisit
    ? sql`from analytics.pageview
      where environment_id = ${query.environmentId}::text and ts >= ${instant(range.start)} and ts < ${instant(range.end)}`
    : sql`from (with ${visitCtes(query, range)} select * from visit_rows) visit_rows where true`;

export type Plan = { readonly days: { readonly first: string; readonly last: string } | null; readonly edges: ReadonlyArray<Range> };

/**
 * Which closed days the rollup answers and which edges are read raw. Rollups hold no filtered rows, so a read with
 * filters is all raw unless `rolled` says its daily rows exist, as {@link totalsKey} does for one session dim value.
 */
export function readPlan(query: StoreQuery, rolledUpTo: string | null, rolled = query.filters.length === 0): Plan {
  const { range, timezone } = query;
  const raw: Plan = { days: null, edges: [range] };
  if (rolledUpTo === null || !rolled) return raw;
  const closed = localDays(timezone, range.start, range.end)
    .filter((day) => dayStart(timezone, day) >= range.start && dayEnd(timezone, day) <= range.end && day <= rolledUpTo);
  const first = closed[0];
  const last = closed[closed.length - 1];
  if (first === undefined || last === undefined) return raw;
  const edges: Range[] = [];
  if (dayStart(timezone, first) > range.start) edges.push({ start: range.start, end: dayStart(timezone, first) });
  if (dayEnd(timezone, last) < range.end) edges.push({ start: dayEnd(timezone, last), end: range.end });
  return { days: { first, last }, edges };
}

export const rolledUpTo = Effect.fn("WebAnalytics.rolledUpTo")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ day: string | null }>(sql`
    select rolled_up_to::text as day from analytics.rollup_state where environment_id = ${environmentId}::text`, "objects");
  return rows[0]?.day ?? null;
});

const planned = Effect.fn("WebAnalytics.plan")(function* (query: StoreQuery, rolled?: boolean) {
  return readPlan(query, yield* rolledUpTo(query.environmentId), rolled);
});

type DailyKey = { readonly dim: string; readonly value: string };

/** The daily rows holding a totals read: the site's own, or with one eq filter on a session dim, that value's visits. */
export function totalsKey(filters: ReadonlyArray<Filter>): DailyKey | null {
  const [only, ...rest] = filters;
  if (only === undefined) return { dim: "", value: "" };
  return rest.length === 0 && only.op === "eq" && SESSION_KEYS.has(only.key) ? { dim: only.key, value: only.value } : null;
}

/** Reads the rollup marker and the rows it selects from one snapshot, so a concurrent rollup or time zone reset can't split them. */
const inSnapshot = <A, E, R>(program: Effect.Effect<A, E, R>) =>
  Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.transaction(program, { isolationLevel: "repeatable read", accessMode: "read only" });
  });

const dailyRows = (query: StoreQuery, days: NonNullable<Plan["days"]>, dim: string, value?: string) => sql`
  from analytics.daily
  where environment_id = ${query.environmentId}::text and dim = ${dim}::text${value === undefined ? sql`` : sql` and value = ${value}::text`}
    and day between ${days.first}::date and ${days.last}::date`;

type TotalsRow = { bucket: string; pageviews: string; visitors: string; visits: string; bounces: string; total_duration: string };
const toBucketRow = (row: TotalsRow): BucketRow => ({
  bucket: new Date(Number(row.bucket)),
  pageviews: Number(row.pageviews),
  visitors: Number(row.visitors),
  visits: Number(row.visits),
  bounces: Number(row.bounces),
  totalDuration: Number(row.total_duration),
});

const unionTotals = (parts: SQL[]) => sql`
  select bucket, sum(pageviews)::bigint as pageviews, sum(visitors)::bigint as visitors, sum(visits)::bigint as visits,
    sum(bounces)::bigint as bounces, sum(total_duration)::bigint as total_duration
  from (${sql.join(parts, sql` union all `)}) parts
  group by bucket`;

export function dailyTotalsSql(query: StoreQuery, plan: Plan) {
  const { timezone } = query;
  const key = totalsKey(query.filters);
  const parts: SQL[] = [];
  if (plan.days !== null && key !== null) {
    parts.push(sql`
      select ${epochMs(sql`(day::timestamp at time zone ${timezone}::text)`)} as bucket,
        pageviews, visitors, visits, bounces, total_duration
      ${dailyRows(query, plan.days, key.dim, key.value)}`);
  }
  for (const edge of plan.edges) {
    parts.push(sql`
      select ${epochMs(sql`(${localDate(timezone)}::timestamp at time zone ${timezone}::text)`)} as bucket,
        count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors,
        0::bigint as visits, 0::bigint as bounces, 0::bigint as total_duration
      ${rowsIn(query, edge)} group by 1`);
    parts.push(sql`
      select ${epochMs(sql`(${localDate(timezone)}::timestamp at time zone ${timezone}::text)`)} as bucket,
        0::bigint as pageviews, 0::bigint as visitors, count(*)::bigint as visits,
        count(*) filter (where pageviews = 1)::bigint as bounces, coalesce(sum(duration), 0)::bigint as total_duration
      from (${sessionsIn(query, edge)}) sessions group by 1`);
  }
  return unionTotals(parts);
}

export const dailyTotals = Effect.fn("WebAnalytics.dailyTotals")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<TotalsRow>(dailyTotalsSql(query, yield* planned(query, totalsKey(query.filters) !== null)), "objects");
  return rows.map(toBucketRow);
}, inSnapshot);

export const hourlyTotals = Effect.fn("WebAnalytics.hourlyTotals")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const { timezone, range } = query;
  if (range.start >= range.end) return [];
  const closedUntil = query.filters.length > 0 ? null : yield* rolledUpTo(query.environmentId);
  const rolledEnd = closedUntil === null ? range.start : new Date(Math.min(Math.max(dayEnd(timezone, closedUntil).getTime(), range.start.getTime()), range.end.getTime()));
  const parts: SQL[] = [];
  if (rolledEnd > range.start) {
    parts.push(sql`
      select ${epochMs(sql`hour`)} as bucket, pageviews, visitors, visits, bounces, total_duration
      from analytics.session_hourly
      where environment_id = ${query.environmentId}::text and hour >= ${instant(range.start)} and hour < ${instant(rolledEnd)}`);
  }
  if (rolledEnd < range.end) {
    const edge = { start: rolledEnd, end: range.end };
    parts.push(sql`
      select ${epochMs(localHour(timezone))} as bucket,
        count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors,
        0::bigint as visits, 0::bigint as bounces, 0::bigint as total_duration
      ${rowsIn(query, edge)} group by 1`);
    parts.push(sql`
      select ${epochMs(localHour(timezone))} as bucket,
        0::bigint as pageviews, 0::bigint as visitors, count(*)::bigint as visits,
        count(*) filter (where pageviews = 1)::bigint as bounces, coalesce(sum(duration), 0)::bigint as total_duration
      from (${sessionsIn(query, edge)}) sessions group by 1`);
  }
  const rows = yield* drizzle.execute<TotalsRow>(unionTotals(parts), "objects");
  return rows.map(toBucketRow);
}, inSnapshot);

type ValueDbRow = { value: string; pageviews: string; visitors: string };

export const breakdown = Effect.fn("WebAnalytics.breakdown")(function* (query: StoreQuery, dim: FilterKey) {
  const { drizzle } = yield* Database;
  const plan = yield* planned(query);
  const parts: SQL[] = [];
  if (plan.days !== null) parts.push(sql`select value, pageviews, visitors ${dailyRows(query, plan.days, dim)}`);
  for (const edge of plan.edges) {
    parts.push(sql`
      select ${column(dim)} as value, count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors
      ${rowsIn(query, edge, SESSION_KEYS.has(dim))} and ${column(dim)} is not null group by ${column(dim)}, ${localDate(query.timezone)}`);
  }
  const rows = yield* drizzle.execute<ValueDbRow>(sql`
    select value, sum(pageviews)::bigint as pageviews, sum(visitors)::bigint as visitors
    from (${sql.join(parts, sql` union all `)}) parts group by value`, "objects");
  return rows.map((row): ValueRow => ({ value: row.value, pageviews: Number(row.pageviews), visitors: Number(row.visitors) }));
}, inSnapshot);

type SessionDbRow = { value: string; visits: string; bounces: string; total_duration: string };

export const sessionsBy = Effect.fn("WebAnalytics.sessionsBy")(function* (query: StoreQuery, dim: SessionDim) {
  const { drizzle } = yield* Database;
  const plan = yield* planned(query);
  const parts: SQL[] = [];
  if (plan.days !== null) parts.push(sql`select value, visits, bounces, total_duration ${dailyRows(query, plan.days, dim)} and visits > 0`);
  for (const edge of plan.edges) {
    parts.push(sql`
      select ${column(dim)} as value, count(*)::bigint as visits, count(*) filter (where pageviews = 1)::bigint as bounces,
        coalesce(sum(duration), 0)::bigint as total_duration
      from (${sessionsIn(query, edge)}) sessions where ${column(dim)} is not null group by ${column(dim)}`);
  }
  const rows = yield* drizzle.execute<SessionDbRow>(sql`
    select value, sum(visits)::bigint as visits, sum(bounces)::bigint as bounces, sum(total_duration)::bigint as total_duration
    from (${sql.join(parts, sql` union all `)}) parts group by value`, "objects");
  return rows.map((row): SessionRow => ({
    value: row.value, visits: Number(row.visits), bounces: Number(row.bounces), totalDuration: Number(row.total_duration),
  }));
}, inSnapshot);

type EntryDbRow = SessionDbRow & { visitors: string };

export const entryPages = Effect.fn("WebAnalytics.entryPages")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const plan = yield* planned(query);
  const parts: SQL[] = [];
  if (plan.days !== null) parts.push(sql`select value, visitors, visits, bounces, total_duration ${dailyRows(query, plan.days, "entry_page")}`);
  for (const edge of plan.edges) {
    parts.push(sql`
      select pathname as value, count(distinct visitor_id)::bigint as visitors, count(*)::bigint as visits,
        count(*) filter (where pageviews = 1)::bigint as bounces, coalesce(sum(duration), 0)::bigint as total_duration
      from (${sessionsIn(query, edge)}) sessions group by pathname, ${localDate(query.timezone)}`);
  }
  const rows = yield* drizzle.execute<EntryDbRow>(sql`
    select value, sum(visitors)::bigint as visitors, sum(visits)::bigint as visits, sum(bounces)::bigint as bounces,
      sum(total_duration)::bigint as total_duration
    from (${sql.join(parts, sql` union all `)}) parts group by value`, "objects");
  return rows.map((row): EntryRow => ({
    value: row.value, visitors: Number(row.visitors), visits: Number(row.visits), bounces: Number(row.bounces), totalDuration: Number(row.total_duration),
  }));
}, inSnapshot);

type GoalDbRow = { goal_id: string; name: string; visitors: string; conversions: string };

/**
 * Only the range's distinct paths meet the goal patterns, then the matched pageviews fold per goal, day and visitor.
 * Unfiltered, the range is scanned twice, which beats spilling a million rows to a CTE; a filtered range is sessionized once.
 */
export const goalConversions = Effect.fn("WebAnalytics.goalConversions")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<GoalDbRow>(sql`
    with range_rows as ${query.filters.length === 0 ? sql`not materialized` : sql`materialized`} (
      select pathname, ts, visitor_id ${rowsIn(query, query.range)}
    ), goals as (
      select id, name, replace(replace(replace(replace(path_pattern, '\\', '\\\\'), '%', '\\%'), '_', '\\_'), '*', '%') as pattern
      from analytics.goal where environment_id = ${query.environmentId}::text
    ), matched as (
      select goals.id as goal_id, paths.pathname
      from goals join (select distinct pathname from range_rows) paths on paths.pathname like goals.pattern escape '\\'
    ), hits as (
      select matched.goal_id, ${localDate(query.timezone)} as day, visitor_id, count(*)::bigint as pageviews
      from matched join range_rows using (pathname)
      group by 1, 2, 3
    ), per_goal as (
      select goal_id, count(*)::bigint as visitors, sum(pageviews)::bigint as conversions from hits group by 1
    )
    select goals.id as goal_id, goals.name, coalesce(per_goal.visitors, 0)::bigint as visitors, coalesce(per_goal.conversions, 0)::bigint as conversions
    from goals left join per_goal on per_goal.goal_id = goals.id`, "objects");
  return rows.map((row): GoalRow => ({ goalId: row.goal_id, name: row.name, visitors: Number(row.visitors), conversions: Number(row.conversions) }));
});
