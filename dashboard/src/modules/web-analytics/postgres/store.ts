import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { dayEnd, dayStart, ENTRY_LOOKBACK_SECONDS, localDays, SESSION_GAP_SECONDS } from "../local-time";
import { type BucketRow, type EntryRow, type Filter, type FilterKey, type GoalRow, type Interval, type Range, SESSION_DIMS, type SessionDim, type SessionRow, type ValueRow } from "../model";

export type StoreQuery = {
  readonly environmentId: string;
  readonly timezone: string;
  readonly range: Range;
  readonly filters: ReadonlyArray<Filter>;
};

export const COLUMN = {
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
const lookback = sql.raw(`interval '${ENTRY_LOOKBACK_SECONDS} seconds'`);
const epochMs = (expr: SQL) => sql`(extract(epoch from ${expr}) * 1000)::bigint`;

export const SESSION_KEYS: ReadonlySet<FilterKey> = new Set(SESSION_DIMS);
export const sessionFilters = (query: StoreQuery) => query.filters.filter((filter) => SESSION_KEYS.has(filter.key));
export const pageFilters = (query: StoreQuery) => query.filters.filter((filter) => !SESSION_KEYS.has(filter.key));

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

const HEAD_DIMS = sql.raw(SESSION_DIMS.map((dim) => `first_value(${dim}) over visit as ${dim}`).join(", "));

/**
 * Sessionizes the pageviews in the range's window, and keeps as `visits` the sessions the filters
 * select: session dim filters test the first pageview, page filters need at least one pageview of the session to pass.
 * The window reaches a session gap before the range for exact session starts, and a day past it for whole sessions.
 * A visitor seen in the gap before the range may be mid-visit, so their earlier pageviews within the entry lookback join
 * the window, and the visit takes its dims from its real first pageview. Every range, a rolled day or a raw read,
 * attributes a visit alike.
 * Only a visitor with a pageview in the window that passes each eq filter can have a selected visit, so only theirs are
 * sessionized: a visitor's visits never depend on another visitor's pageviews.
 * Each pageview takes its visit's first values and counts from window functions, never a join: the planner has no row
 * estimates for a CTE, and with a filter it picked nested loops that went quadratic.
 * A read of one of `visits` or `visit_rows` inlines the sessionizing; `shared` sessionizes once for a read of both.
 */
export const visitCtes = (query: StoreQuery, range: Range, shared = false) => {
  const picks = pageFilters(query);
  const selected = sql`${filterClause(sessionFilters(query))}${picks.length > 0 ? sql` and picked > 0` : sql``}`;
  const candidates = sql.join(query.filters.filter((filter) => filter.op === "eq").map((filter) => sql`
      and visitor_id in (
        select visitor_id from analytics.pageview
        where environment_id = ${query.environmentId}::text
          and ts >= ${instant(range.start)} - ${lookback} and ts < ${instant(range.end)} + interval '1 day'${filterClause([filter])})`), sql``);
  return sql`
  window_rows as (
    select ts, visitor_id, host, pathname, source, utm_source, utm_medium, utm_campaign, utm_term, utm_content, language
    from analytics.pageview
    where environment_id = ${query.environmentId}::text
      and ts >= ${instant(range.start)} - ${gap}
      and ts < ${instant(range.end)} + interval '1 day'${candidates}
    union all
    select ts, visitor_id, host, pathname, source, utm_source, utm_medium, utm_campaign, utm_term, utm_content, language
    from analytics.pageview
    where environment_id = ${query.environmentId}::text
      and ts >= ${instant(range.start)} - ${lookback}
      and ts < ${instant(range.start)} - ${gap}
      and visitor_id in (
        select visitor_id from analytics.pageview
        where environment_id = ${query.environmentId}::text and ts >= ${instant(range.start)} - ${gap} and ts < ${instant(range.start)})${candidates}
  ), marked as (
    select *, case when ts - lag(ts) over w <= ${gap} then 0 else 1 end as is_new
    from window_rows window w as (partition by visitor_id order by ts, pathname, host)
  ), numbered as (
    select *, sum(is_new) over (partition by visitor_id order by ts, pathname, host rows unbounded preceding) as sn
    from marked
  ), attributed as ${shared ? sql`materialized` : sql`not materialized`} (
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
 * Without session filters a pageview passing the page filters is selected by its own visit, so reads that need only its
 * own columns skip sessionizing.
 */
const rowsIn = (query: StoreQuery, range: Range, needsVisit = false) =>
  sessionFilters(query).length === 0 && !needsVisit
    ? sql`from analytics.pageview
      where environment_id = ${query.environmentId}::text and ts >= ${instant(range.start)} and ts < ${instant(range.end)}${filterClause(query.filters)}`
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

/** The last closed day and the closed days that late pageviews have changed since. */
const rollupMarker = Effect.fn("WebAnalytics.rollupMarker")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ day: string | null; dirty: string[] }>(sql`
    select rolled_up_to::text as day, dirty::text[] as dirty from analytics.rollup_state where environment_id = ${environmentId}::text`, "objects");
  return { rolledUpTo: rows[0]?.day ?? null, dirty: rows[0]?.dirty ?? [] };
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
  group by bucket order by bucket`;

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
  const marker = query.filters.length > 0 ? null : yield* rollupMarker(query.environmentId);
  const closedUntil = marker?.rolledUpTo ?? null;
  const rolledEnd = closedUntil === null ? range.start : new Date(Math.min(Math.max(dayEnd(timezone, closedUntil).getTime(), range.start.getTime()), range.end.getTime()));
  const parts: SQL[] = [];
  if (rolledEnd > range.start) {
    // A dirty day's hours keep their stale visits until rebuilt, but count its pageviews and visitors raw.
    const dirty = (marker?.dirty ?? [])
      .map((day) => ({ start: new Date(Math.max(dayStart(timezone, day).getTime(), range.start.getTime())), end: new Date(Math.min(dayEnd(timezone, day).getTime(), rolledEnd.getTime())) }))
      .filter((part) => part.start < part.end);
    const inDirty = dirty.length === 0
      ? sql`false`
      : sql.join(dirty.map((part) => sql`(hour >= ${instant(part.start)} and hour < ${instant(part.end)})`), sql` or `);
    parts.push(sql`
      select ${epochMs(sql`hour`)} as bucket,
        case when ${inDirty} then 0 else pageviews end as pageviews, case when ${inDirty} then 0 else visitors end as visitors,
        visits, bounces, total_duration
      from analytics.session_hourly
      where environment_id = ${query.environmentId}::text and hour >= ${instant(range.start)} and hour < ${instant(rolledEnd)}`);
    for (const part of dirty) {
      parts.push(sql`
        select ${epochMs(localHour(timezone))} as bucket,
          count(*)::bigint as pageviews, count(distinct visitor_id)::bigint as visitors,
          0::bigint as visits, 0::bigint as bounces, 0::bigint as total_duration
        ${rowsIn(query, part)} group by 1`);
    }
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
const toValueRow = (row: ValueDbRow): ValueRow => ({ value: row.value, pageviews: Number(row.pageviews), visitors: Number(row.visitors) });

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
  return rows.map(toValueRow);
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
const toEntryRow = (row: EntryDbRow): EntryRow => ({
  value: row.value, visitors: Number(row.visitors), visits: Number(row.visits), bounces: Number(row.bounces), totalDuration: Number(row.total_duration),
});

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
  return rows.map(toEntryRow);
}, inSnapshot);

type GoalDbRow = { goal_id: string; name: string; visitors: string; conversions: string };
const toGoalRow = (row: GoalDbRow): GoalRow => ({ goalId: row.goal_id, name: row.name, visitors: Number(row.visitors), conversions: Number(row.conversions) });

/** The goal CTEs over `range_rows`, ending in `goal_rows`: only its distinct paths meet the goal patterns, then the matched pageviews fold per goal, day and visitor. */
const goalCtes = (query: StoreQuery) => sql`
  goals as (
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
  ), goal_rows as (
    select goals.id as goal_id, goals.name, coalesce(per_goal.visitors, 0)::bigint as visitors, coalesce(per_goal.conversions, 0)::bigint as conversions
    from goals left join per_goal on per_goal.goal_id = goals.id
  )`;

/** Without session filters, the range is scanned twice, which beats spilling a million rows to a CTE; with them it is sessionized once. */
export const goalConversions = Effect.fn("WebAnalytics.goalConversions")(function* (query: StoreQuery) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<GoalDbRow>(sql`
    with range_rows as ${sessionFilters(query).length === 0 ? sql`not materialized` : sql`materialized`} (
      select pathname, ts, visitor_id ${rowsIn(query, query.range)}
    ), ${goalCtes(query)}
    select * from goal_rows`, "objects");
  return rows.map(toGoalRow);
});

export type Figures = {
  readonly daily: ReadonlyArray<BucketRow>;
  readonly hourly: ReadonlyArray<BucketRow>;
  readonly values: ReadonlyMap<FilterKey, ReadonlyArray<ValueRow>>;
  readonly entries: ReadonlyArray<EntryRow>;
  readonly goals: ReadonlyArray<GoalRow>;
};

type FiguresDbRow = {
  figures: {
    daily: TotalsRow[];
    hourly: TotalsRow[];
    values: Record<string, ValueDbRow[]>;
    entries: EntryDbRow[];
    goals: GoalDbRow[];
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
 * and its goals. Rollups hold no filtered rows, so with filters each figure reads raw, and one statement sessionizes the
 * range once for all of them; without, each reads its rollup. Each visitor count groups by visitor before it counts,
 * which Postgres hashes, where `count(distinct)` sorts the range again for every figure.
 */
export const figures = Effect.fn("WebAnalytics.figures")(function* (query: StoreQuery, interval: Interval, dims: ReadonlyArray<FilterKey>) {
  if (query.filters.length === 0) {
    const [daily, hourly, values, entries, goals] = yield* Effect.all([
      dailyTotals(query),
      interval === "hour" ? hourlyTotals(query) : Effect.succeed([]),
      Effect.all(dims.map((dim) => breakdown(query, dim).pipe(Effect.map((rows) => [dim, rows] as const))), { concurrency: "unbounded" }),
      entryPages(query),
      goalConversions(query),
    ], { concurrency: "unbounded" });
    return { daily, hourly, values: new Map(values), entries, goals } satisfies Figures;
  }
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
    range_rows as (select pathname, ts, visitor_id from visit_rows),
    ${goalCtes(query)}
    select jsonb_build_object(
      'daily', ${asJson(sharedTotals(epochMs(sql`(${day}::timestamp at time zone ${timezone}::text)`)))},
      'hourly', ${interval === "hour" ? asJson(sharedTotals(epochMs(localHour(timezone)))) : sql`'[]'::jsonb`},
      'values', jsonb_build_object(${sql.join(breakdowns, sql`, `)}),
      'entries', ${asJson(sql`
        select value, count(*)::bigint as visitors, sum(visits)::bigint as visits, sum(bounces)::bigint as bounces,
          sum(total_duration)::bigint as total_duration from (
          select pathname as value, count(*)::bigint as visits,
            count(*) filter (where pageviews = 1)::bigint as bounces, coalesce(sum(duration), 0)::bigint as total_duration
          from range_visits group by pathname, ${day}, visitor_id
        ) visitor_days group by value`)},
      'goals', ${asJson(sql`select * from goal_rows`)}
    ) as figures`, "objects");
  const result = row?.figures;
  return {
    daily: (result?.daily ?? []).map(toBucketRow),
    hourly: (result?.hourly ?? []).map(toBucketRow),
    values: new Map(dims.map((dim) => [dim, (result?.values[dim] ?? []).map(toValueRow)])),
    entries: (result?.entries ?? []).map(toEntryRow),
    goals: (result?.goals ?? []).map(toGoalRow),
  } satisfies Figures;
});
