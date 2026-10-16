import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { ENTRY_LOOKBACK_SECONDS, SESSION_GAP_SECONDS } from "../local-time";
import { type BucketRow, type EntryRow, type Filter, type FilterKey, type GoalRow, type Interval, type Range, SESSION_DIMS, type ValueRow } from "../model";

export type StoreQuery = {
  readonly environmentId: string;
  readonly timezone: string;
  readonly range: Range;
  readonly filters: ReadonlyArray<Filter>;
};

const COLUMN = {
  page: "path",
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
const lookback = sql.raw(`interval '${ENTRY_LOOKBACK_SECONDS} seconds'`);
const epochMs = (expr: SQL) => sql`(extract(epoch from ${expr}) * 1000)::bigint`;

const SESSION_KEYS: ReadonlySet<FilterKey> = new Set(SESSION_DIMS);
const sessionFilters = (query: StoreQuery) => query.filters.filter((filter) => SESSION_KEYS.has(filter.key));
const pageFilters = (query: StoreQuery) => query.filters.filter((filter) => !SESSION_KEYS.has(filter.key));

/** `neq` keeps rows whose value is missing: a pageview with no source is not from Google. */
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

/** The site's counted pageviews: a pageview with no visitor came on a day the Machine had no salt for. */
const pageviews = (query: StoreQuery) =>
  sql`analytics.request where environment_id = ${query.environmentId}::text and kind = 'pageview' and visitor_id is not null`;

const HEAD_DIMS = sql.raw(SESSION_DIMS.map((dim) => `first_value(${dim}) over visit as ${dim}`).join(", "));

/**
 * Sessionizes the pageviews in the range's window, and keeps as `visits` the sessions the filters
 * select: session dim filters test the first pageview, page filters need at least one pageview of the session to pass.
 * The window reaches a session gap before the range for exact session starts, and a day past it for whole sessions.
 * A visitor seen in the gap before the range may be mid-visit, so their earlier pageviews within the entry lookback join
 * the window, and the visit takes its dims from its real first pageview.
 * Only a visitor with a pageview in the window that passes each eq filter can have a selected visit, so only theirs are
 * sessionized: a visitor's visits never depend on another visitor's pageviews.
 * Each pageview takes its visit's first values and counts from window functions, never a join: the planner has no row
 * estimates for a CTE, and with a filter it picked nested loops that went quadratic.
 * A read of one of `visits` or `visit_rows` inlines the sessionizing; `shared` sessionizes once for a read of both.
 */
const visitCtes = (query: StoreQuery, range: Range, shared = false) => {
  const picks = pageFilters(query);
  const selected = sql`${filterClause(sessionFilters(query))}${picks.length > 0 ? sql` and picked > 0` : sql``}`;
  const candidates = sql.join(query.filters.filter((filter) => filter.op === "eq").map((filter) => sql`
      and visitor_id in (
        select visitor_id from ${pageviews(query)}
          and ts >= ${instant(range.start)} - ${lookback} and ts < ${instant(range.end)} + interval '1 day'${filterClause([filter])})`), sql``);
  return sql`
  window_rows as (
    select ts, visitor_id, host, path, source, utm_source, utm_medium, utm_campaign, utm_term, utm_content, language
    from ${pageviews(query)}
      and ts >= ${instant(range.start)} - ${gap}
      and ts < ${instant(range.end)} + interval '1 day'${candidates}
    union all
    select ts, visitor_id, host, path, source, utm_source, utm_medium, utm_campaign, utm_term, utm_content, language
    from ${pageviews(query)}
      and ts >= ${instant(range.start)} - ${lookback}
      and ts < ${instant(range.start)} - ${gap}
      and visitor_id in (
        select visitor_id from ${pageviews(query)} and ts >= ${instant(range.start)} - ${gap} and ts < ${instant(range.start)})${candidates}
  ), marked as (
    select *, case when ts - lag(ts) over w <= ${gap} then 0 else 1 end as is_new
    from window_rows window w as (partition by visitor_id order by ts, path, host)
  ), numbered as (
    select *, sum(is_new) over (partition by visitor_id order by ts, path, host rows unbounded preceding) as sn
    from marked
  ), attributed as ${shared ? sql`materialized` : sql`not materialized`} (
    select ts, visitor_id, path, is_new, ${HEAD_DIMS}, max(ts) over visit as ended, count(*) over visit as pageviews,
      count(*) filter (where true${filterClause(picks)}) over visit as picked
    from numbered
    window visit as (partition by visitor_id, sn order by ts, path, host rows between unbounded preceding and unbounded following)
  ), visits as (
    select *, floor(extract(epoch from (ended - ts)))::bigint as duration
    from attributed where is_new = 1${selected}
  ), visit_rows as (
    select ts, visitor_id, path, ${sql.raw(SESSION_DIMS.join(", "))}
    from attributed
    where ts >= ${instant(range.start)} and ts < ${instant(range.end)}${selected}${filterClause(picks)}
  )`;
};

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

type ValueDbRow = { value: string; pageviews: string; visitors: string };
const toValueRow = (row: ValueDbRow): ValueRow => ({ value: row.value, pageviews: Number(row.pageviews), visitors: Number(row.visitors) });

type SessionDbRow = { value: string; visits: string; bounces: string; total_duration: string };
type EntryDbRow = SessionDbRow & { visitors: string };
const toEntryRow = (row: EntryDbRow): EntryRow => ({
  value: row.value, visitors: Number(row.visitors), visits: Number(row.visits), bounces: Number(row.bounces), totalDuration: Number(row.total_duration),
});

type GoalDbRow = { goal_id: string; name: string; visitors: string; conversions: string };
const toGoalRow = (row: GoalDbRow): GoalRow => ({ goalId: row.goal_id, name: row.name, visitors: Number(row.visitors), conversions: Number(row.conversions) });

/** The goal CTEs over `range_rows`, ending in `goal_rows`: only its distinct paths meet the goal patterns, then the matched pageviews fold per goal, day and visitor. */
const goalCtes = (query: StoreQuery) => sql`
  goals as (
    select id, name, replace(replace(replace(replace(path_pattern, '\\', '\\\\'), '%', '\\%'), '_', '\\_'), '*', '%') as pattern
    from analytics.goal where environment_id = ${query.environmentId}::text
  ), matched as (
    select goals.id as goal_id, paths.path
    from goals join (select distinct path from range_rows) paths on paths.path like goals.pattern escape '\\'
  ), hits as (
    select matched.goal_id, ${localDate(query.timezone)} as day, visitor_id, count(*)::bigint as pageviews
    from matched join range_rows using (path)
    group by 1, 2, 3
  ), per_goal as (
    select goal_id, count(*)::bigint as visitors, sum(pageviews)::bigint as conversions from hits group by 1
  ), goal_rows as (
    select goals.id as goal_id, goals.name, coalesce(per_goal.visitors, 0)::bigint as visitors, coalesce(per_goal.conversions, 0)::bigint as conversions
    from goals left join per_goal on per_goal.goal_id = goals.id
  )`;

export type Figures = {
  readonly daily: ReadonlyArray<BucketRow>;
  readonly hourly: ReadonlyArray<BucketRow>;
  readonly values: ReadonlyMap<FilterKey, ReadonlyArray<ValueRow>>;
  readonly entries: ReadonlyArray<EntryRow>;
  readonly goals: ReadonlyArray<GoalRow>;
  readonly noSaltDays: ReadonlyArray<string>;
};

type FiguresDbRow = {
  figures: {
    daily: TotalsRow[];
    hourly: TotalsRow[];
    values: Record<string, ValueDbRow[]>;
    entries: EntryDbRow[];
    goals: GoalDbRow[];
    no_salt: { day: string }[];
  };
};

const asJson = (rows: SQL) => sql`(select coalesce(jsonb_agg(rows), '[]'::jsonb) from (${rows}) rows)`;

/** A range's totals by bucket from the shared `visit_rows` and `range_visits`. */
const sharedTotals = (bucket: SQL) => unionTotals([
  sql`select bucket, sum(pageviews)::bigint as pageviews, count(*)::bigint as visitors,
    0::bigint as visits, 0::bigint as bounces, 0::bigint as total_duration
    from (select ${bucket} as bucket, count(*)::bigint as pageviews from visit_rows group by 1, visitor_id) visitors group by 1`,
  sql`select ${bucket} as bucket, 0::bigint as pageviews, 0::bigint as visitors, count(*)::bigint as visits,
    count(*) filter (where pageviews = 1)::bigint as bounces, coalesce(sum(duration), 0)::bigint as total_duration
    from range_visits group by 1`,
]);

/**
 * Every figure of one range: its day series, its hour series for an hour interval, a breakdown per dim, its entry pages
 * its goals, and the days whose pageviews came without a salt. One statement sessionizes the range once for all of them. Each visitor count groups by visitor before it counts,
 * which Postgres hashes, where `count(distinct)` sorts the range again for every figure.
 */
export const figures = Effect.fn("WebAnalytics.figures")(function* (query: StoreQuery, interval: Interval, dims: ReadonlyArray<FilterKey>) {
  const { drizzle } = yield* Database;
  const { range, timezone } = query;
  const day = localDate(timezone);
  const breakdowns = dims.map((dim) => sql`${dim}::text, ${asJson(sql`
    select value, sum(pageviews)::bigint as pageviews, count(*)::bigint as visitors from (
      select ${column(dim)} as value, count(*)::bigint as pageviews
      from visit_rows where ${column(dim)} is not null group by ${column(dim)}, ${day}, visitor_id
    ) visitor_days group by value`)}`);
  const [row] = yield* drizzle.execute<FiguresDbRow>(sql`
    with ${visitCtes(query, range, true)},
    range_visits as (select * from visits where ts >= ${instant(range.start)} and ts < ${instant(range.end)}),
    range_rows as (select path, ts, visitor_id from visit_rows),
    ${goalCtes(query)}
    select jsonb_build_object(
      'daily', ${asJson(sharedTotals(epochMs(sql`(${day}::timestamp at time zone ${timezone}::text)`)))},
      'hourly', ${interval === "hour" ? asJson(sharedTotals(epochMs(localHour(timezone)))) : sql`'[]'::jsonb`},
      'values', jsonb_build_object(${sql.join(breakdowns, sql`, `)}),
      'entries', ${asJson(sql`
        select value, count(*)::bigint as visitors, sum(visits)::bigint as visits, sum(bounces)::bigint as bounces,
          sum(total_duration)::bigint as total_duration from (
          select path as value, count(*)::bigint as visits,
            count(*) filter (where pageviews = 1)::bigint as bounces, coalesce(sum(duration), 0)::bigint as total_duration
          from range_visits group by path, ${day}, visitor_id
        ) visitor_days group by value`)},
      'goals', ${asJson(sql`select * from goal_rows`)},
      'no_salt', ${asJson(sql`
        select distinct ${day}::text as day from analytics.request
        where environment_id = ${query.environmentId}::text and kind = 'pageview' and visitor_id is null
          and ts >= ${instant(range.start)} and ts < ${instant(range.end)}
        order by 1`)}
    ) as figures`, "objects");
  const result = row?.figures;
  return {
    daily: (result?.daily ?? []).map(toBucketRow),
    hourly: (result?.hourly ?? []).map(toBucketRow),
    values: new Map(dims.map((dim) => [dim, (result?.values[dim] ?? []).map(toValueRow)])),
    entries: (result?.entries ?? []).map(toEntryRow),
    goals: (result?.goals ?? []).map(toGoalRow),
    noSaltDays: (result?.no_salt ?? []).map((row) => row.day),
  } satisfies Figures;
});
