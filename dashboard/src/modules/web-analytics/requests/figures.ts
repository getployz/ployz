import { Effect, Schema } from "effect";
import { dayStart, hourStart, localDay, SESSION_GAP_SECONDS } from "../local-time";
import {
  type BucketRow, type EntryRow, type Filter, type FilterKey, type GoalRow, type Interval, type Range, SESSION_DIMS, type ValueRow,
} from "../model";
import { botHashes, PAGEVIEW } from "./bots";
import { Params, readRequests, type Scope } from "./statement";

export type FigureQuery = Scope & {
  readonly timezone: string;
  readonly range: Range;
  readonly filters: ReadonlyArray<Filter>;
  /** The Environment's goals, each a path pattern where `*` matches anything. */
  readonly goals: ReadonlyArray<{ readonly id: string; readonly name: string; readonly pathPattern: string }>;
};

export type Figures = {
  readonly daily: ReadonlyArray<BucketRow>;
  readonly hourly: ReadonlyArray<BucketRow>;
  readonly values: ReadonlyMap<FilterKey, ReadonlyArray<ValueRow>>;
  readonly entries: ReadonlyArray<EntryRow>;
  readonly goals: ReadonlyArray<GoalRow>;
  readonly noSaltDays: ReadonlyArray<string>;
};

/** The columns of a counted pageview (`base` below) a filter or a breakdown names. */
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

const SESSION_KEYS: ReadonlySet<FilterKey> = new Set(SESSION_DIMS);
/** A session is attributed by its first pageview's values: its `h_` columns. */
const head = (key: FilterKey) => (SESSION_KEYS.has(key) ? `h_${COLUMN[key]}` : COLUMN[key]);

/** A dim whose empty value means the request carried none, so it is not a value to count. Host and source keep their empty value (Direct). */
const OPTIONAL_DIMS: ReadonlySet<FilterKey> = new Set(["utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content", "language"]);

const GAP_US = SESSION_GAP_SECONDS * 1_000_000;

/**
 * The days a range's visits can live in. A visitor's id changes with the day's salt, so a visit never crosses a
 * site-local midnight, and the local days holding the range hold every visit that touches it.
 */
export function sessionWindow(timezone: string, range: Range): Range {
  return {
    start: dayStart(timezone, localDay(timezone, range.start)),
    end: dayStart(timezone, localDay(timezone, new Date(range.end.getTime() - 1)), 1),
  };
}

/** An instant as microseconds since the epoch, bound as a value. */
const micros = (params: Params, instant: Date) => params.int(instant.getTime() * 1000);

const filterSql = (params: Params, filters: ReadonlyArray<Filter>, column: (key: FilterKey) => string) =>
  filters.map((filter) => `${column(filter.key)} ${filter.op === "eq" ? "=" : "!="} ${params.string(filter.value)}`);

/** `neq` keeps rows whose value is missing: a pageview with no source is not from Google. */
const sessionFilters = (query: FigureQuery) => query.filters.filter((filter) => SESSION_KEYS.has(filter.key));
const pageFilters = (query: FigureQuery) => query.filters.filter((filter) => !SESSION_KEYS.has(filter.key));

/** Rows of one visitor are ordered the same way everywhere: by time, then page, then host. */
const ORDER = "us, path, host";
const FRAME = "ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING";

/**
 * The counted pageviews of the sessions window, sessionized. `base` is every counted pageview (a pageview with no
 * visitor came on a day the Machine had no salt, and is not counted), `pv` only the visitors a filter can select,
 * `numbered` gives each pageview its visit's number, and `visits` folds each visit with the values of its first pageview,
 * keeping the ones the filters select: session filters test the first pageview, page filters need a pageview of the visit
 * that passes. Only a visitor with a pageview passing each eq filter can have a selected visit, so only theirs are
 * sessionized; a visitor's visits never depend on another visitor's pageviews.
 */
const visitCtes = (query: FigureQuery, params: Params, window: Range, bots: ReadonlyArray<string>) => {
  const { range } = query;
  const picks = pageFilters(query);
  const row = (key: FilterKey) => COLUMN[key];
  const candidates = query.filters.filter((filter) => filter.op === "eq")
    .map((filter) => `visitor IN (SELECT visitor FROM base WHERE ${filterSql(params, [filter], row)[0]})`);
  const picked = picks.length === 0 ? "true" : filterSql(params, picks, row).join(" AND ");
  const selected = [
    ...filterSql(params, sessionFilters(query), head),
    ...(picks.length === 0 ? [] : ["picked > 0"]),
  ];
  const from = micros(params, range.start);
  const to = micros(params, range.end);
  // The entry page is the first pageview's path, though `page` is not a session dim.
  const heads = [...SESSION_DIMS.map((dim) => COLUMN[dim]), COLUMN.page].map((column) => `argMin(${column}, (${ORDER})) AS h_${column}`).join(", ");
  return [
    `base AS (
      SELECT toUnixTimestamp64Micro(Timestamp) AS us, assumeNotNull(Visitor) AS visitor, Host AS host, Path AS path,
        Language AS language, UtmSource AS utm_source, UtmMedium AS utm_medium, UtmCampaign AS utm_campaign,
        UtmTerm AS utm_term, UtmContent AS utm_content,
        if(UtmSource != '', UtmSource, if(Ref != '', Ref, ReferrerHost)) AS source
      FROM scoped
      WHERE Timestamp >= ${params.instant(window.start)} AND Timestamp < ${params.instant(window.end)}
        AND Visitor IS NOT NULL AND ${PAGEVIEW} AND cityHash64(Ua) NOT IN ${params.hashes(bots)})`,
    `pv AS (SELECT * FROM base${candidates.length === 0 ? "" : ` WHERE ${candidates.join(" AND ")}`})`,
    `marked AS (
      SELECT *, lagInFrame(us, 1, 0) OVER w AS prev
      FROM pv WINDOW w AS (PARTITION BY visitor ORDER BY ${ORDER} ${FRAME}))`,
    `numbered AS (
      SELECT *, sum(if(prev = 0 OR us - prev > ${GAP_US}, 1, 0)) OVER (PARTITION BY visitor ORDER BY ${ORDER} ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS sn
      FROM marked)`,
    `visits AS (
      SELECT visitor, sn, min(us) AS start_us, max(us) AS end_us, count() AS n, ${heads},
        countIf(us >= ${from} AND us < ${to} AND ${picked}) AS pv, countIf(${picked}) AS picked
      FROM numbered GROUP BY visitor, sn${selected.length === 0 ? "" : ` HAVING ${selected.join(" AND ")}`})`,
  ];
};

/** The pageviews of selected visits that fall in the range and pass the page filters, as `rows`. */
const rowsCte = (query: FigureQuery, params: Params) => {
  const { range } = query;
  const picks = pageFilters(query);
  const inRange = `us >= ${micros(params, range.start)} AND us < ${micros(params, range.end)}`;
  const page = picks.length === 0 ? "" : ` AND ${filterSql(params, picks, (key) => COLUMN[key]).join(" AND ")}`;
  if (query.filters.length === 0) return `rows AS (SELECT us, visitor, path FROM pv WHERE ${inRange})`;
  return `rows AS (SELECT us, visitor, path FROM numbered WHERE ${inRange}${page} AND (visitor, sn) IN (SELECT visitor, sn FROM visits))`;
};

const Count = Schema.Finite;
const GroupRow = Schema.Struct({
  kind: Schema.String,
  key: Schema.String,
  pageviews: Count,
  seen: Count,
  starters: Count,
  visits: Count,
  bounces: Count,
  lasted: Count,
});

/**
 * Every visit-level figure in one pass over `visits`: each visit fans out to its day, its entry page and one value per
 * requested dim, and one GROUP BY counts them all. No output alias repeats a column name an aggregate reads: ClickHouse would
 * substitute the aggregate for the column. A visitor is one id per site-local day, so distinct visitors count
 * by id.
 */
const groupedFigures = (query: FigureQuery, dims: ReadonlyArray<FilterKey>, timezone: string, params: Params) => {
  const { range } = query;
  const fan = [
    `('daily', toString(toDate(fromUnixTimestamp64Micro(start_us), ${params.string(timezone)})))`,
    "('entry', h_path)",
    ...dims.filter((dim) => dim !== "page").map((dim) => `('${dim}', ${head(dim)})`),
  ];
  const hidden = dims.filter((dim) => OPTIONAL_DIMS.has(dim)).map((dim) => `'${dim}'`);
  return `
    SELECT kind, key, sum(pv) AS pageviews, uniqExactIf(visitor, pv > 0) AS seen, uniqExactIf(visitor, started) AS starters,
      countIf(started) AS visits, countIf(started AND n = 1) AS bounces, sumIf(duration, started) AS lasted
    FROM (
      SELECT visitor, pv, n, duration, started, g.1 AS kind, g.2 AS key FROM (
        SELECT *, start_us >= ${micros(params, range.start)} AND start_us < ${micros(params, range.end)} AS started,
          intDiv(end_us - start_us, 1000000) AS duration
        FROM visits) ARRAY JOIN [${fan.join(", ")}] AS g)
    ${hidden.length === 0 ? "" : `WHERE NOT (kind IN (${hidden.join(", ")}) AND key = '')`}
    GROUP BY kind, key
    HAVING kind = 'daily' OR (kind = 'entry' AND visits > 0) OR (kind NOT IN ('daily', 'entry') AND pageviews > 0)`;
};

const PageRow = Schema.Struct({ key: Schema.String, pageviews: Count, visitors: Count });
const GoalCount = Schema.Struct({ id: Schema.String, visitors: Count, conversions: Count });
const HourRow = Schema.Struct({ hour: Count, pageviews: Count, visitors: Count, visits: Count, bounces: Count, duration: Count });
const DayRow = Schema.Struct({ day: Schema.String });

/** A goal's pattern as a LIKE: `*` is anything and every other character stands for itself. */
export const likePattern = (pattern: string) => pattern.replace(/[\\%_]/g, (char) => `\\${char}`).replaceAll("*", "%");

/** The first instant of the hour of `us`, in seconds, as the site-local wall clock sees it. */
const hourKey = (us: string, zone: string) => {
  const seconds = `intDiv(${us}, 1000000)`;
  return `(${seconds} - (((${seconds} + timeZoneOffset(toDateTime(${seconds}, ${zone}))) % 3600) + 3600) % 3600)`;
};

const toBucket = (bucket: Date, row: { pageviews: number; visitors: number; visits: number; bounces: number; duration: number }): BucketRow =>
  ({ bucket, pageviews: row.pageviews, visitors: row.visitors, visits: row.visits, bounces: row.bounces, totalDuration: row.duration });

/**
 * Every figure of one range: its day series, its hour series for an hour interval, a breakdown per dim, its entry pages,
 * its goals, and the days whose pageviews came without a salt. The visit-level figures share one sessionization; the
 * page breakdown, goals and hours read the pageviews of the selected visits, each in a statement of its own, run together.
 */
export const figures = Effect.fn("WebAnalytics.figures")(function* (query: FigureQuery, interval: Interval, dims: ReadonlyArray<FilterKey>) {
  const { range, timezone } = query;
  const window = sessionWindow(timezone, range);
  const bots = yield* botHashes(query, window, true);

  const grouped = Effect.gen(function* () {
    const params = new Params();
    const ctes = visitCtes(query, params, window, bots);
    return yield* readRequests(query, params, ctes, groupedFigures(query, dims, timezone, params), GroupRow);
  });

  const pages = Effect.gen(function* () {
    if (!dims.includes("page")) return [];
    const params = new Params();
    const ctes = [...visitCtes(query, params, window, bots), rowsCte(query, params)];
    return yield* readRequests(query, params, ctes, "SELECT path AS key, count() AS pageviews, uniqExact(visitor) AS visitors FROM rows GROUP BY path", PageRow);
  });

  const goals = Effect.gen(function* () {
    if (query.goals.length === 0) return [];
    const params = new Params();
    const ctes = [
      ...visitCtes(query, params, window, bots),
      rowsCte(query, params),
      `goals AS (SELECT arrayJoin(arrayZip(${params.strings(query.goals.map((goal) => goal.id))}, ${params.strings(query.goals.map((goal) => likePattern(goal.pathPattern)))})) AS goal)`,
      "matched AS (SELECT goal.1 AS id, path FROM goals CROSS JOIN (SELECT DISTINCT path FROM rows) AS paths WHERE path LIKE goal.2)",
    ];
    return yield* readRequests(query, params, ctes,
      "SELECT id, uniqExact(visitor) AS visitors, count() AS conversions FROM rows INNER JOIN matched USING (path) GROUP BY id", GoalCount);
  });

  const hours = Effect.gen(function* () {
    if (interval !== "hour") return [];
    const params = new Params();
    const zone = params.string(timezone);
    const ctes = [...visitCtes(query, params, window, bots), rowsCte(query, params)];
    const part = (pageviews: string, visitors: string, visits: string, bounces: string, duration: string, hour: string, source: string, where = "") =>
      `SELECT ${hour} AS hour, ${pageviews} AS pageviews, ${visitors} AS visitors, ${visits} AS visits, ${bounces} AS bounces, ${duration} AS duration FROM ${source} ${where} GROUP BY hour`;
    return yield* readRequests(query, params, ctes, `
      ${part("count()", "uniqExact(visitor)", "0", "0", "0", hourKey("us", zone), "rows")}
      UNION ALL
      ${part("0", "0", "countIf(started)", "countIf(started AND n = 1)", "sumIf(intDiv(end_us - start_us, 1000000), started)", hourKey("start_us", zone),
        `(SELECT *, start_us >= ${micros(params, range.start)} AND start_us < ${micros(params, range.end)} AS started FROM visits)`, "WHERE started")}`, HourRow);
  });

  const noSalt = Effect.gen(function* () {
    const params = new Params();
    return yield* readRequests(query, params, [], `
      SELECT DISTINCT toString(toDate(Timestamp, ${params.string(timezone)})) AS day FROM scoped
      WHERE Timestamp >= ${params.instant(range.start)} AND Timestamp < ${params.instant(range.end)}
        AND Visitor IS NULL AND ${PAGEVIEW} AND cityHash64(Ua) NOT IN ${params.hashes(bots)}
      ORDER BY day`, DayRow);
  });

  const [groups, pageRows, goalRows, hourRows, noSaltRows] = yield* Effect.all([grouped, pages, goals, hours, noSalt], { concurrency: "unbounded" });

  const daily = groups.filter((row) => row.kind === "daily")
    .map((row) => toBucket(dayStart(timezone, row.key), { ...row, visitors: row.seen, duration: row.lasted }));
  const entries = groups.filter((row) => row.kind === "entry")
    .map((row): EntryRow => ({ value: row.key, visitors: row.starters, visits: row.visits, bounces: row.bounces, totalDuration: row.lasted }));
  const values = new Map<FilterKey, ReadonlyArray<ValueRow>>(dims.map((dim) => [
    dim,
    dim === "page"
      ? pageRows.map((row) => ({ value: row.key, pageviews: row.pageviews, visitors: row.visitors }))
      : groups.filter((row) => row.kind === dim).map((row) => ({ value: row.key, pageviews: row.pageviews, visitors: row.seen })),
  ]));
  const counted = new Map(goalRows.map((row) => [row.id, row]));
  return {
    daily,
    // An hour's rows can arrive keyed by two instants where a zone changes offset inside it; the series sums those.
    hourly: hourRows.map((row) => toBucket(hourStart(timezone, new Date(row.hour * 1000)), row)),
    values,
    entries,
    goals: query.goals.map((goal): GoalRow => ({
      goalId: goal.id, name: goal.name, visitors: counted.get(goal.id)?.visitors ?? 0, conversions: counted.get(goal.id)?.conversions ?? 0,
    })),
    noSaltDays: noSaltRows.map((row) => row.day),
  } satisfies Figures;
});

const SeenRow = Schema.Struct({ day: Schema.String, counted: Count });

/**
 * Whether the site ever counted a visitor in the days kept, and when it has not, the days whose pageviews all came
 * without a salt.
 */
export const seenVisitors = Effect.fn("WebAnalytics.seen")(function* (query: Scope & { readonly timezone: string; readonly kept: Range }) {
  const bots = yield* botHashes(query, query.kept, true);
  const params = new Params();
  const rows = yield* readRequests(query, params, [], `
    SELECT toString(toDate(Timestamp, ${params.string(query.timezone)})) AS day, countIf(Visitor IS NOT NULL) AS counted FROM scoped
    WHERE Timestamp >= ${params.instant(query.kept.start)} AND Timestamp < ${params.instant(query.kept.end)}
      AND ${PAGEVIEW} AND cityHash64(Ua) NOT IN ${params.hashes(bots)}
    GROUP BY day ORDER BY day`, SeenRow);
  return { seen: rows.some((row) => row.counted > 0), lostDays: rows.filter((row) => row.counted === 0).map((row) => row.day) };
});
