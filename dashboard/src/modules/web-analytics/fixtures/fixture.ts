import { Effect } from "effect";
import * as compute from "../compute";
import type { Filter, FilterKey, GoalRow, Interval, Range, SessionDim, ValueRow } from "../model";
import * as store from "../postgres/store";
import { type GoalRecord, type PageviewRecord, Reference } from "./reference";

/** One question asked of a site, in the shape the adapter asks it. Range bounds are ISO instants on local hour starts. */
export type QuerySpec = { readonly name: string; readonly range: readonly [string, string]; readonly filters: ReadonlyArray<Filter> } & (
  | { readonly kind: "overview" }
  | { readonly kind: "timeseries"; readonly interval: Interval }
  | { readonly kind: "breakdown"; readonly dim: FilterKey; readonly limit: number }
  | { readonly kind: "sessions"; readonly dim: SessionDim; readonly limit: number }
  | { readonly kind: "entryPages"; readonly limit: number }
  | { readonly kind: "goalConversions" }
);

/** A result as JSON keeps it: every `Date` is its ISO string. */
export type Serialized<T> = T extends Date ? string : T extends object ? { readonly [K in keyof T]: Serialized<T[K]> } : T;

export type Answer =
  | compute.Overview
  | ReadonlyArray<compute.Bucket>
  | ReadonlyArray<ValueRow>
  | ReadonlyArray<compute.SessionResult>
  | ReadonlyArray<compute.EntryResult>
  | ReadonlyArray<GoalRow>;

export type FixtureSite = { readonly environmentId: string; readonly timezone: string };

/** A site's rows and goals, the instant "now" the rollup runs at, and every query with the answer the reference gives. */
export type Fixture = {
  readonly now: string;
  readonly site: FixtureSite;
  readonly rows: ReadonlyArray<PageviewRecord>;
  readonly goals: ReadonlyArray<GoalRecord>;
  readonly queries: ReadonlyArray<{ readonly spec: QuerySpec; readonly expected: Serialized<Answer> }>;
};

export const rangeOf = (spec: QuerySpec): Range => ({ start: new Date(spec.range[0]), end: new Date(spec.range[1]) });

/** What the reference says the store must answer, through the same compute step the adapter applies. */
export function expectedOf(spec: QuerySpec, timezone: string, rows: ReadonlyArray<PageviewRecord>, goals: ReadonlyArray<GoalRecord>): Answer {
  const range = rangeOf(spec);
  const reference = new Reference(timezone, rows, range, spec.filters);
  switch (spec.kind) {
    case "overview":
      return compute.overview(reference.dailyTotals());
    case "timeseries":
      return compute.timeseries(spec.interval === "hour" ? reference.hourlyTotals() : reference.dailyTotals(), range, timezone, spec.interval);
    case "breakdown":
      return compute.breakdown(reference.breakdown(spec.dim), spec.limit);
    case "sessions":
      return compute.sessions(reference.sessionsBy(spec.dim), spec.limit);
    case "entryPages":
      return compute.entryPages(reference.entryPages(), spec.limit);
    case "goalConversions":
      return compute.goalConversions(reference.goalConversions(goals));
  }
}

/** The store's answer to `spec`, through the same compute step the adapter applies. */
export const storeAnswer = Effect.fn("WebAnalytics.fixtureAnswer")(function* (spec: QuerySpec, site: FixtureSite) {
  const range = rangeOf(spec);
  const query: store.StoreQuery = { environmentId: site.environmentId, timezone: site.timezone, range, filters: spec.filters };
  switch (spec.kind) {
    case "overview":
      return compute.overview(yield* store.dailyTotals(query));
    case "timeseries":
      return compute.timeseries(
        spec.interval === "hour" ? yield* store.hourlyTotals(query) : yield* store.dailyTotals(query),
        range,
        site.timezone,
        spec.interval,
      );
    case "breakdown":
      return compute.breakdown(yield* store.breakdown(query, spec.dim), spec.limit);
    case "sessions":
      return compute.sessions(yield* store.sessionsBy(query, spec.dim), spec.limit);
    case "entryPages":
      return compute.entryPages(yield* store.entryPages(query), spec.limit);
    case "goalConversions":
      return compute.goalConversions(yield* store.goalConversions(query));
  }
});

/** A result as the fixture file keeps it, so a store answer and a stored expectation compare alike. */
export const serialized = <A>(answer: A): Serialized<A> => JSON.parse(JSON.stringify(answer));
