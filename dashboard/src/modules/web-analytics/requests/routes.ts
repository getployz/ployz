import { Effect, Schema } from "effect";
import { hourStart, retentionCutoff } from "../local-time";
import type { Range } from "../model";
import { notBot } from "./bots";
import { Params, readRequests, type Scope } from "./statement";

export type RouteQuery = Scope & {
  readonly timezone: string;
  readonly range: Range;
  readonly service?: string;
};

export const ROUTE_LIMIT = 200;
export const OTHER_ROUTE = "Other";
const SLOWEST = 20;
const FAILURES = 50;

export type RouteRow = {
  readonly route: string;
  readonly requests: number;
  readonly errors: number;
  readonly p50: number;
  readonly p95: number;
  readonly p99: number;
};

export type RequestSample = {
  readonly requestId: string;
  readonly ts: Date;
  readonly service: string;
  readonly method: string;
  readonly path: string;
  readonly status: number;
  readonly durationMs: number;
};

export type StatusBucket = { readonly bucket: Date; readonly clientErrors: number; readonly serverErrors: number };
export type RouteStatus = { readonly route: string; readonly status: number; readonly requests: number };

const idlike = (x: string) => `(match(${x}, '^[0-9]+$')
  OR match(${x}, '(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$')
  OR (match(${x}, '(?i)^[0-9a-f]{12,}$') AND match(${x}, '[0-9]'))
  OR (match(${x}, '^[A-Za-z0-9_-]{16,}$') AND match(${x}, '[0-9]') AND match(${x}, '[A-Za-z]')))`;

const EXTENSION = "[.]([A-Za-z0-9]{1,5})$";

/** One path segment as rule r2 reads it, with `s` the lambda's segment. */
const segment = (() => {
  const stem = `extract(s, '^(.+)${EXTENSION}')`;
  const ext = `extract(s, '${EXTENSION}')`;
  return `multiIf(
    match(s, '^.+${EXTENSION}'), if(${idlike(stem)} OR (match(${stem}, '[0-9]') AND lengthUTF8(${stem}) >= 6),
      concat(':id.', ${ext}), concat(replaceRegexpOne(${stem}, '-[A-Za-z0-9_]{6,}$', '-:hash'), '.', ${ext})),
    ${idlike("s")} OR match(s, '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'), ':id',
    s)`;
})();

/**
 * Rule r2: a path with its ids, dates and content hashes replaced, so `/users/42` and `/users/7` are one route. Read
 * time, so a rule change reshapes every stored day.
 */
export const routeOf = (path: string) =>
  `coalesce(nullIf(arrayStringConcat(arrayMap(s -> ${segment}, splitByChar('/', splitByChar('?', ${path})[1])), '/'), ''), '/')`;

const Count = Schema.Finite;
const Sample = Schema.Struct({
  requestId: Schema.String, ts: Count, service: Schema.String, method: Schema.String, path: Schema.String, status: Count, durationMs: Count,
});
type SampleRow = typeof Sample.Type;

const toSample = (row: SampleRow): RequestSample => ({ ...row, ts: new Date(row.ts) });

const SAMPLE_COLUMNS = "RequestId AS requestId, toUnixTimestamp64Milli(Timestamp) AS ts, Service AS service, Method AS method, Path AS path, Status AS status, duration_ms AS durationMs";

const requestsCte = (query: RouteQuery, params: Params, service: string | undefined) => `
  rows AS (
    SELECT ${routeOf("Path")} AS route, Timestamp, RequestId, Service, Method, Path, Status, DurationUs / 1000 AS duration_ms
    FROM scoped
    WHERE Timestamp >= ${params.instant(query.range.start)} AND Timestamp < ${params.instant(query.range.end)}
      AND ${notBot(params)}${service === undefined ? "" : ` AND Service = ${params.string(service)}`})`;

const TOP = `top AS (SELECT route FROM rows GROUP BY route ORDER BY count() DESC, route LIMIT ${ROUTE_LIMIT})`;
const TOP_FAILED = `top AS (SELECT route FROM rows WHERE Status >= 400 GROUP BY route ORDER BY count() DESC, route LIMIT ${ROUTE_LIMIT})`;

const named = `if(route IN (SELECT route FROM top), route, '${OTHER_ROUTE}')`;

const ServiceRow = Schema.Struct({ service: Schema.String });

/** The Services that took a request in range, whatever Service a view is narrowed to. */
const services = Effect.fn("WebAnalytics.services")(function* (query: RouteQuery) {
  const params = new Params();
  const rows = yield* readRequests(query, params, [requestsCte(query, params, undefined)],
    "SELECT DISTINCT Service AS service FROM rows ORDER BY service", ServiceRow);
  return rows.map((row) => row.service);
});

const RouteAnswer = Schema.Struct({ route: Schema.String, requests: Count, errors: Count, quantiles: Schema.Array(Count) });

export const api = Effect.fn("WebAnalytics.api")(function* (query: RouteQuery) {
  const run = <Row>(select: (params: Params) => string, row: Schema.Decoder<Row>, withTop = false) => Effect.gen(function* () {
    const params = new Params();
    return yield* readRequests(query, params, [requestsCte(query, params, query.service), ...(withTop ? [TOP] : [])], select(params), row);
  });
  const [routes, slowest, serviceNames] = yield* Effect.all([
    run(() => `
      SELECT ${named} AS route, count() AS requests, countIf(Status >= 500) AS errors,
        quantilesExactInclusive(0.5, 0.95, 0.99)(duration_ms) AS quantiles
      FROM rows GROUP BY route ORDER BY requests DESC, route`, RouteAnswer, true),
    run(() => `SELECT ${SAMPLE_COLUMNS} FROM rows ORDER BY duration_ms DESC, Timestamp DESC LIMIT ${SLOWEST}`, Sample),
    services(query),
  ], { concurrency: "unbounded" });
  return {
    routes: routes.map((row): RouteRow => ({
      route: row.route, requests: row.requests, errors: row.errors, p50: row.quantiles[0] ?? 0, p95: row.quantiles[1] ?? 0, p99: row.quantiles[2] ?? 0,
    })),
    slowest: slowest.map(toSample),
    services: serviceNames,
  };
});

const HourAnswer = Schema.Struct({ hour: Count, client: Count, server: Count });
const FailedRoute = Schema.Struct({ route: Schema.String, status: Count, requests: Count });

export const errors = Effect.fn("WebAnalytics.errors")(function* (query: RouteQuery) {
  const run = <Row>(select: (params: Params) => string, row: Schema.Decoder<Row>, withTop = false) => Effect.gen(function* () {
    const params = new Params();
    return yield* readRequests(query, params, [requestsCte(query, params, query.service), ...(withTop ? [TOP_FAILED] : [])], select(params), row);
  });
  const seconds = "intDiv(toUnixTimestamp64Milli(Timestamp), 1000)";
  const [hourly, byRoute, newest, serviceNames] = yield* Effect.all([
    run((params) => `
      SELECT ${seconds} - (((${seconds} + timeZoneOffset(toDateTime(${seconds}, ${params.string(query.timezone)}))) % 3600) + 3600) % 3600 AS hour,
        countIf(Status < 500) AS client, countIf(Status >= 500) AS server
      FROM rows WHERE Status >= 400 GROUP BY hour ORDER BY hour`, HourAnswer),
    run(() => `
      SELECT ${named} AS route, Status AS status, count() AS requests
      FROM rows WHERE Status >= 400 GROUP BY route, status ORDER BY requests DESC, route, status`, FailedRoute, true),
    run(() => `SELECT ${SAMPLE_COLUMNS} FROM rows WHERE Status >= 400 ORDER BY Timestamp DESC LIMIT ${FAILURES}`, Sample),
    services(query),
  ], { concurrency: "unbounded" });
  return {
    hourly: hourly.map((row): StatusBucket => ({ bucket: hourStart(query.timezone, new Date(row.hour * 1000)), clientErrors: row.client, serverErrors: row.server })),
    byRoute: byRoute.map((row): RouteStatus => ({ route: row.route, status: row.status, requests: row.requests })),
    newest: newest.map(toSample),
    services: serviceNames,
  };
});

export const request = Effect.fn("WebAnalytics.request")(function* (query: Scope & { readonly timezone: string }, requestId: string, now: number) {
  const params = new Params();
  const [row] = yield* readRequests(query, params, [], `
    SELECT RequestId AS requestId, toUnixTimestamp64Milli(Timestamp) AS ts, Service AS service, Method AS method, Path AS path,
      Status AS status, DurationUs / 1000 AS durationMs
    FROM scoped
    WHERE Timestamp >= ${params.instant(retentionCutoff(now))} AND RequestId = ${params.string(requestId)}
    ORDER BY Timestamp DESC LIMIT 1`, Sample);
  return row === undefined ? null : toSample(row);
});
