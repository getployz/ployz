import { Clock, Effect } from "effect";
import { Validation } from "#/server/public-error";
import * as compute from "./compute";
import { isHourStart, retentionFloor } from "./local-time";
import type { FilterKey, Interval, Query, Range, Site } from "./model";
import * as routes from "./postgres/routes";
import * as store from "./postgres/store";

export * from "./model";
export type { Bucket, EntryResult, Overview } from "./compute";
export type { RequestSample, RouteRow, RouteStatus, StatusBucket } from "./postgres/routes";

/** Every answer names the range it covers; `clamped` says retention cut the asked-for start. */
export type Answered = { readonly range: Range; readonly clamped: boolean };

const storeQuery = (q: Query, range: Range): store.StoreQuery =>
  ({ environmentId: q.site.environmentId, timezone: q.site.timezone, range, filters: q.filters });

const resolve = Effect.fn("WebAnalytics.resolve")(function* (q: Query) {
  const { timezone } = q.site;
  for (const [field, bound] of [["start", q.range.start], ["end", q.range.end]] as const) {
    if (!isHourStart(timezone, bound)) {
      return yield* new Validation({ message: `Range ${field} must be the start of a ${timezone} hour.`, field });
    }
  }
  if (q.range.start >= q.range.end) return yield* new Validation({ message: "Range start must be before its end.", field: "start" });
  const floor = retentionFloor(timezone, yield* Clock.currentTimeMillis);
  const start = q.range.start < floor ? new Date(Math.min(floor.getTime(), q.range.end.getTime())) : q.range.start;
  const range: Range = { start, end: q.range.end };
  return { range, clamped: start.getTime() !== q.range.start.getTime(), store: storeQuery(q, range) };
});

/** Every figure of one range the Web view shows, with a breakdown per dim; one sessionization serves them all. */
export const figures = Effect.fn("WebAnalytics.figures")(function* <Dim extends FilterKey>(q: Query, interval: Interval, dims: ReadonlyArray<Dim>) {
  const { range, clamped, store: sq } = yield* resolve(q);
  const rows = yield* store.figures(sq, interval, dims);
  const values = new Map(dims.map((dim) => [dim, compute.breakdown(rows.values.get(dim) ?? [], q.limit)]));
  return {
    range,
    clamped,
    // A range's days sum to its totals, but its hours don't: a visitor seen in two hours is one visitor.
    overview: compute.overview(rows.daily),
    buckets: compute.timeseries(interval === "hour" ? rows.hourly : rows.daily, range, q.site.timezone, interval),
    values: (dim: Dim) => values.get(dim) ?? [],
    entryPages: compute.entryPages(rows.entries, q.limit),
    goals: compute.goalConversions(rows.goals),
    noSaltDays: rows.noSaltDays,
  };
});

export const api = Effect.fn("WebAnalytics.api")(function* (q: Query, service?: string) {
  const { range, clamped, store: sq } = yield* resolve(q);
  return { range, clamped, ...(yield* routes.api({ ...sq, ...(service !== undefined && { service }) })) };
});

export const errors = Effect.fn("WebAnalytics.errors")(function* (q: Query, service?: string) {
  const { range, clamped, store: sq } = yield* resolve(q);
  return { range, clamped, ...(yield* routes.errors({ ...sq, ...(service !== undefined && { service }) })) };
});

export const request = (site: Site, requestId: string) => routes.request(site.environmentId, requestId);
