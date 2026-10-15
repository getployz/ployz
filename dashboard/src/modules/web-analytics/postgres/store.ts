import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { dayEnd, dayStart, localDays, SESSION_GAP_SECONDS } from "../local-time";
import type { BucketRow, EntryRow, Filter, FilterKey, GoalRow, Range, SessionDim, SessionRow, ValueRow } from "../model";

/** What the store needs to answer one question: whose rows, in which clock, over which instants, selected how. */
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
/** The instant at which the site-local hour holding `ts` starts. Subtracting the wall-clock seconds past the hour stays
 * exact through a repeated DST hour, where truncating a wall time would pick one of its two instants. */
export const localHour = (timezone: string) =>
  sql`ts - make_interval(secs => mod(extract(epoch from (ts at time zone ${timezone}::text)), 3600)::double precision)`;
const gap = sql.raw(`interval '${SESSION_GAP_SECONDS} seconds'`);
const epochMs = (expr: SQL) => sql`(extract(epoch from ${expr}) * 1000)::bigint`;

/** `neq` keeps rows with no value at all, so it is "is distinct from" rather than "<>". */
const filterClause = (filters: ReadonlyArray<Filter>) =>
  sql.join(
    filters.map((filter) =>
      filter.op === "eq"
        ? sql` and ${column(filter.key)} = ${filter.value}::text`
        : sql` and ${column(filter.key)} is distinct from ${filter.value}::text`),
    sql``,
  );

const rowsIn = (query: StoreQuery, range: Range) =>
  sql`from analytics.pageview
    where environment_id = ${query.environmentId}::text and ts >= ${instant(range.start)} and ts < ${instant(range.end)}${filterClause(query.filters)}`;

/**
 * Sessions whose first pageview lies in `range`, one row per session with that first pageview's columns plus `ended`
 * and `pageviews`. Reads `[start - 1800 s, end + 1 day)` so a session is cut only by its gaps, never by the range.
 */
export const sessionsIn = (query: StoreQuery, range: Range) => sql`
  with window_rows as (
    select ts, visitor_id, host, pathname, source, utm_source, utm_medium, utm_campaign, utm_term, utm_content, language
    from analytics.pageview
    where environment_id = ${query.environmentId}::text
      and ts >= ${instant(range.start)} - ${gap}
      and ts < ${instant(range.end)} + interval '1 day'${filterClause(query.filters)}
  ), marked as (
    select *, case when ts - lag(ts) over w <= ${gap} then 0 else 1 end as is_new
    from window_rows window w as (partition by visitor_id order by ts, pathname, host)
  ), numbered as (
    select *, sum(is_new) over (partition by visitor_id order by ts, pathname, host rows unbounded preceding) as sn
    from marked
  ), spans as (
    select visitor_id, sn, max(ts) as ended, count(*)::bigint as pageviews from numbered group by visitor_id, sn
  )
  select head.*, spans.ended, spans.pageviews,
    floor(extract(epoch from (spans.ended - head.ts)))::bigint as duration
  from numbered head join spans using (visitor_id, sn)
  where head.is_new = 1 and head.ts >= ${instant(range.start)} and head.ts < ${instant(range.end)}`;

export type Plan = { readonly days: { readonly first: string; readonly last: string } | null; readonly edges: ReadonlyArray<Range> };

/**
 * Whole site-local days inside the range that the rollup has closed come from `daily`; at most two raw edges cover the
 * rest. Filters select rows the rollup never saw, so a filtered query reads everything raw.
 */
export function readPlan(query: StoreQuery, rolledUpTo: string | null): Plan {
  const { range, timezone } = query;
  const raw: Plan = { days: null, edges: [range] };
  if (rolledUpTo === null || query.filters.length > 0) return raw;
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

const planned = Effect.fn("WebAnalytics.plan")(function* (query: StoreQuery) {
  return readPlan(query, yield* rolledUpTo(query.environmentId));
});

const dailyRows = (query: StoreQuery, days: NonNullable<Plan["days"]>, dim: string) => sql`
  from analytics.daily
  where environment_id = ${query.environmentId}::text and dim = ${dim}::text
    and day between ${days.first}::date and ${days.last}::date`;

const num = (value: string) => Number(value);

type TotalsRow = { bucket: string; pageviews: string; visitors: string; visits: string; bounces: string; total_duration: string };
const toBucketRow = (row: TotalsRow): BucketRow => ({
  bucket: new Date(num(row.bucket)),
  pageviews: num(row.pageviews),
  visitors: num(row.visitors),
  visits: num(row.visits),
  bounces: num(row.bounces),
  totalDuration: num(row.total_duration),
});

const unionTotals = (parts: SQL[]) => sql`
  select bucket, sum(pageviews)::bigint as pageviews, sum(visitors)::bigint as visitors, sum(visits)::bigint as visits,
    sum(bounces)::bigint as bounces, sum(total_duration)::bigint as total_duration
  from (${sql.join(parts, sql` union all `)}) parts
  group by bucket`;

/** The SQL behind `dailyTotals`, kept apart so a perf test can `explain analyze` the query the adapter runs. */
export function dailyTotalsSql(query: StoreQuery, plan: Plan) {
  const { timezone } = query;
  const parts: SQL[] = [];
  if (plan.days !== null) {
    parts.push(sql`
      select ${epochMs(sql`(day::timestamp at time zone ${timezone}::text)`)} as bucket,
        pageviews, visitors, visits, bounces, total_duration
      ${dailyRows(query, plan.days, "")}`);
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

/** Totals per site-local day, the bucket being the instant the day starts. */
export const dailyTotals = Effect.fn("WebAnalytics.dailyTotals")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<TotalsRow>(dailyTotalsSql(query, yield* planned(query)), "objects");
  return rows.map(toBucketRow);
});

/**
 * Totals per site-local hour. `daily` has no hours, so pageviews read raw across the range; sessions come from
 * `session_hourly` up to the end of the last closed day and from the raw sessionizer after it.
 */
export const hourlyTotals = Effect.fn("WebAnalytics.hourlyTotals")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const { timezone, range } = query;
  const closedUntil = query.filters.length > 0 ? null : yield* rolledUpTo(query.environmentId);
  const rolledEnd = closedUntil === null ? range.start : new Date(Math.min(Math.max(dayEnd(timezone, closedUntil).getTime(), range.start.getTime()), range.end.getTime()));
  const parts: SQL[] = [sql`
    select ${epochMs(localHour(timezone))} as bucket,
      count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors,
      0::bigint as visits, 0::bigint as bounces, 0::bigint as total_duration
    ${rowsIn(query, range)} group by 1`];
  if (rolledEnd > range.start) {
    parts.push(sql`
      select ${epochMs(sql`hour`)} as bucket, 0::bigint as pageviews, 0::bigint as visitors, visits, bounces, total_duration
      from analytics.session_hourly
      where environment_id = ${query.environmentId}::text and hour >= ${instant(range.start)} and hour < ${instant(rolledEnd)}`);
  }
  if (rolledEnd < range.end) {
    parts.push(sql`
      select ${epochMs(localHour(timezone))} as bucket,
        0::bigint as pageviews, 0::bigint as visitors, count(*)::bigint as visits,
        count(*) filter (where pageviews = 1)::bigint as bounces, coalesce(sum(duration), 0)::bigint as total_duration
      from (${sessionsIn(query, { start: rolledEnd, end: range.end })}) sessions group by 1`);
  }
  const rows = yield* drizzle.execute<TotalsRow>(unionTotals(parts), "objects");
  return rows.map(toBucketRow);
});

type ValueDbRow = { value: string; pageviews: string; visitors: string };

/** Pageviews and per-day distinct visitors per value of `dim`, unordered and unlimited. */
export const breakdown = Effect.fn("WebAnalytics.breakdown")(function* (query: StoreQuery, dim: FilterKey) {
  const { drizzle } = yield* Database;
  const plan = yield* planned(query);
  const parts: SQL[] = [];
  if (plan.days !== null) parts.push(sql`select value, pageviews, visitors ${dailyRows(query, plan.days, dim)}`);
  for (const edge of plan.edges) {
    parts.push(sql`
      select ${column(dim)} as value, count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors
      ${rowsIn(query, edge)} and ${column(dim)} is not null group by ${column(dim)}, ${localDate(query.timezone)}`);
  }
  const rows = yield* drizzle.execute<ValueDbRow>(sql`
    select value, sum(pageviews)::bigint as pageviews, sum(visitors)::bigint as visitors
    from (${sql.join(parts, sql` union all `)}) parts group by value`, "objects");
  return rows.map((row): ValueRow => ({ value: row.value, pageviews: num(row.pageviews), visitors: num(row.visitors) }));
});

type SessionDbRow = { value: string; visits: string; bounces: string; total_duration: string };

/** Sessions per value of `dim` on their first pageview, unordered and unlimited. */
export const sessionsBy = Effect.fn("WebAnalytics.sessionsBy")(function* (query: StoreQuery, dim: SessionDim) {
  const { drizzle } = yield* Database;
  const plan = yield* planned(query);
  const parts: SQL[] = [];
  if (plan.days !== null) parts.push(sql`select value, visits, bounces, total_duration ${dailyRows(query, plan.days, dim)}`);
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
    value: row.value, visits: num(row.visits), bounces: num(row.bounces), totalDuration: num(row.total_duration),
  }));
});

type EntryDbRow = SessionDbRow & { visitors: string };

/** Sessions per entry page with the per-day distinct visitors that started them, unordered and unlimited. */
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
    value: row.value, visitors: num(row.visitors), visits: num(row.visits), bounces: num(row.bounces), totalDuration: num(row.total_duration),
  }));
});

type GoalDbRow = { goal_id: string; name: string; visitors: string; conversions: string };

/** Every goal of the site with its matching pageviews and per-day distinct visitors over the range, read raw. */
export const goalConversions = Effect.fn("WebAnalytics.goalConversions")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<GoalDbRow>(sql`
    select goal.id as goal_id, goal.name, coalesce(sum(hits.visitors), 0)::bigint as visitors, coalesce(sum(hits.pageviews), 0)::bigint as conversions
    from analytics.goal goal
    left join lateral (
      select count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors
      ${rowsIn(query, query.range)}
        and pathname like replace(replace(replace(replace(goal.path_pattern, '\\', '\\\\'), '%', '\\%'), '_', '\\_'), '*', '%') escape '\\'
      group by ${localDate(query.timezone)}
    ) hits on true
    where goal.environment_id = ${query.environmentId}::text
    group by goal.id, goal.name`, "objects");
  return rows.map((row): GoalRow => ({ goalId: row.goal_id, name: row.name, visitors: num(row.visitors), conversions: num(row.conversions) }));
});
