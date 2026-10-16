import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import type { Range } from "../model";
import { instant, localHour } from "./store";

export type RouteQuery = {
  readonly environmentId: string;
  readonly timezone: string;
  readonly range: Range;
  readonly service?: string;
};

export const ROUTE_LIMIT = 200;
export const OTHER_ROUTE = "Other";
const SLOWEST = 20;
const FAILURES = 50;

const idlike = (x: SQL) => sql`(${x} ~ '^[0-9]+$'
  or ${x} ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
  or (${x} ~* '^[0-9a-f]{12,}$' and ${x} ~ '[0-9]')
  or (${x} ~ '^[A-Za-z0-9_-]{16,}$' and ${x} ~ '[0-9]' and ${x} ~ '[A-Za-z]'))`;

const EXTENSION = "[.]([A-Za-z0-9]{1,5})$";
const stem = sql.raw(`substring(seg from '^(.+)${EXTENSION}')`);
const extension = sql.raw(`substring(seg from '${EXTENSION}')`);

/**
 * Rule r2: a path with its ids, dates and content hashes replaced, so `/users/42` and `/users/7` are one route. Read
 * time, so a rule change reshapes every stored day.
 */
const route = sql`coalesce(nullif((
  select string_agg(case
    when seg ~ '^.+${sql.raw(EXTENSION)}' then case
      when ${idlike(stem)} or (${stem} ~ '[0-9]' and length(${stem}) >= 6) then ':id.' || ${extension}
      else regexp_replace(${stem}, '-[A-Za-z0-9_]{6,}$', '-:hash') || '.' || ${extension} end
    when ${idlike(sql`seg`)} or seg ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$' then ':id'
    else seg end, '/' order by n)
  from unnest(string_to_array(split_part(path, '?', 1), '/')) with ordinality as segments(seg, n)
), ''), '/')`;

/** Every request in range a person or a program made; bot traffic is left out. */
const requests = (query: RouteQuery) => sql`
  select ${route} as route, ts, request_id, service, method, path, status, duration_ms
  from analytics.request
  where environment_id = ${query.environmentId}::text and kind <> 'bot'
    and ts >= ${instant(query.range.start)} and ts < ${instant(query.range.end)}
    ${query.service === undefined ? sql`` : sql`and service = ${query.service}::text`}`;

const topRoutes = sql`top as (select route from rows group by route order by count(*) desc, route limit ${sql.raw(String(ROUTE_LIMIT))})`;
const named = sql`coalesce(top.route, ${OTHER_ROUTE}::text)`;

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

type SampleDbRow = { request_id: string; ts: string; service: string; method: string; path: string; status: number; duration_ms: number };

const toSample = (row: SampleDbRow): RequestSample => ({
  requestId: row.request_id,
  ts: new Date(row.ts),
  service: row.service,
  method: row.method,
  path: row.path,
  status: row.status,
  durationMs: Number(row.duration_ms),
});

const sampleColumns = sql`request_id, ts, service, method, path, status, duration_ms`;

/** The busiest routes with their latency percentiles and 5xx count, the rest as Other, and the slowest requests. */
export const api = Effect.fn("WebAnalytics.api")(function* (query: RouteQuery) {
  const { drizzle } = yield* Database;
  const routes = yield* drizzle.execute<{ route: string; requests: string; errors: string; p50: number; p95: number; p99: number }>(sql`
    with rows as (${requests(query)}), ${topRoutes}
    select ${named} as route, count(*) as requests, count(*) filter (where status >= 500) as errors,
      percentile_cont(0.5) within group (order by duration_ms) as p50,
      percentile_cont(0.95) within group (order by duration_ms) as p95,
      percentile_cont(0.99) within group (order by duration_ms) as p99
    from rows left join top using (route)
    group by 1 order by requests desc, route`, "objects");
  const slowest = yield* drizzle.execute<SampleDbRow>(sql`
    select ${sampleColumns} from (${requests(query)}) rows order by duration_ms desc, ts desc limit ${sql.raw(String(SLOWEST))}`, "objects");
  return {
    routes: routes.map((row): RouteRow => ({
      route: row.route,
      requests: Number(row.requests),
      errors: Number(row.errors),
      p50: Number(row.p50),
      p95: Number(row.p95),
      p99: Number(row.p99),
    })),
    slowest: slowest.map(toSample),
  };
});

/** 4xx and 5xx counts per site-local hour, failures by route and status, and the newest failures. */
export const errors = Effect.fn("WebAnalytics.errors")(function* (query: RouteQuery) {
  const { drizzle } = yield* Database;
  const hourly = yield* drizzle.execute<{ bucket: string; client: string; server: string }>(sql`
    select ${localHour(query.timezone)} as bucket,
      count(*) filter (where status < 500) as client, count(*) filter (where status >= 500) as server
    from (${requests(query)}) rows where status >= 400 group by 1 order by 1`, "objects");
  const byRoute = yield* drizzle.execute<{ route: string; status: number; requests: string }>(sql`
    with rows as (select * from (${requests(query)}) failed where status >= 400), ${topRoutes}
    select ${named} as route, status, count(*) as requests
    from rows left join top using (route)
    group by 1, 2 order by requests desc, route, status`, "objects");
  const newest = yield* drizzle.execute<SampleDbRow>(sql`
    select ${sampleColumns} from (${requests(query)}) rows where status >= 400
    order by ts desc limit ${sql.raw(String(FAILURES))}`, "objects");
  return {
    hourly: hourly.map((row): StatusBucket => ({ bucket: new Date(row.bucket), clientErrors: Number(row.client), serverErrors: Number(row.server) })),
    byRoute: byRoute.map((row): RouteStatus => ({ route: row.route, status: row.status, requests: Number(row.requests) })),
    newest: newest.map(toSample),
  };
});

export const request = Effect.fn("WebAnalytics.request")(function* (environmentId: string, requestId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<SampleDbRow>(sql`
    select ${sampleColumns} from analytics.request
    where environment_id = ${environmentId}::text and request_id = ${requestId}::text
    order by ts desc limit 1`, "objects");
  return row === undefined ? null : toSample(row);
});
