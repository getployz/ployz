import { eq } from "drizzle-orm";
import { Clock, Effect } from "effect";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Validation } from "#/server/public-error";
import * as clickhouseStore from "./clickhouse/store";
import * as compute from "./compute";
import { isHourStart, retentionFloor } from "./local-time";
import type { FilterKey, Interval, Query, Range, SessionDim } from "./model";
import * as store from "./postgres/store";
import { analyticsClickhouseSite } from "./tables";

export * from "./model";
export type { Bucket, EntryResult, Overview, SessionResult } from "./compute";

/** Every answer names the range it covers; `clamped` says retention cut the asked-for start. */
export type Answered = { readonly range: Range; readonly clamped: boolean };

const storeQuery = (q: Query, range: Range): store.StoreQuery =>
  ({ environmentId: q.site.environmentId, timezone: q.site.timezone, range, filters: q.filters });

/** A site reads ClickHouse only where Cloud reads it at all and the site's backfill has passed its parity check. */
const storeFor = Effect.fn("WebAnalytics.storeFor")(function* (environmentId: string) {
  const { clickhouse } = yield* AppConfig;
  if (clickhouse === null || !clickhouse.reads) return store;
  const { drizzle } = yield* Database;
  const [site] = yield* drizzle.select({ reads: analyticsClickhouseSite.reads }).from(analyticsClickhouseSite)
    .where(eq(analyticsClickhouseSite.environmentId, environmentId));
  return site?.reads === true ? clickhouseStore : store;
});

const resolve = Effect.fn("WebAnalytics.resolve")(function* (q: Query) {
  const { timezone } = q.site;
  for (const [field, bound] of [["start", q.range.start], ["end", q.range.end]] as const) {
    if (!isHourStart(timezone, bound)) {
      return yield* new Validation({ message: `Range ${field} must be the start of a ${timezone} hour.`, field });
    }
  }
  if (q.range.start >= q.range.end) return yield* new Validation({ message: "Range start must be before its end.", field: "start" });
  const floor = retentionFloor(timezone, yield* Clock.currentTimeMillis, q.site.retentionMonths);
  const start = q.range.start < floor ? new Date(Math.min(floor.getTime(), q.range.end.getTime())) : q.range.start;
  const range: Range = { start, end: q.range.end };
  return { range, clamped: start.getTime() !== q.range.start.getTime(), store: storeQuery(q, range), stores: yield* storeFor(q.site.environmentId) };
});

export const overview = Effect.fn("WebAnalytics.overview")(function* (q: Query) {
  const { range, clamped, store: sq, stores } = yield* resolve(q);
  return { range, clamped, totals: compute.overview(yield* stores.dailyTotals(sq)) };
});

export const timeseries = Effect.fn("WebAnalytics.timeseries")(function* (q: Query, interval: Interval) {
  const { range, clamped, store: sq, stores } = yield* resolve(q);
  const rows = interval === "hour" ? yield* stores.hourlyTotals(sq) : yield* stores.dailyTotals(sq);
  return { range, clamped, buckets: compute.timeseries(rows, range, q.site.timezone, interval) };
});

export const breakdown = Effect.fn("WebAnalytics.breakdown")(function* (q: Query, dim: FilterKey) {
  const { range, clamped, store: sq, stores } = yield* resolve(q);
  return { range, clamped, values: compute.breakdown(yield* stores.breakdown(sq, dim), q.limit) };
});

export const topPages = (q: Query) => breakdown(q, "page");
export const sources = (q: Query) => breakdown(q, "source");

export const sessions = Effect.fn("WebAnalytics.sessions")(function* (q: Query, dim: SessionDim) {
  const { range, clamped, store: sq, stores } = yield* resolve(q);
  return { range, clamped, values: compute.sessions(yield* stores.sessionsBy(sq, dim), q.limit) };
});

export const entryPages = Effect.fn("WebAnalytics.entryPages")(function* (q: Query) {
  const { range, clamped, store: sq, stores } = yield* resolve(q);
  return { range, clamped, values: compute.entryPages(yield* stores.entryPages(sq), q.limit) };
});

export const goalConversions = Effect.fn("WebAnalytics.goalConversions")(function* (q: Query) {
  const { range, clamped, store: sq, stores } = yield* resolve(q);
  return { range, clamped, goals: compute.goalConversions(yield* stores.goalConversions(sq)) };
});
