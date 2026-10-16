import { Schema } from "effect";
import { addDays, dayStart, HOUR_MS, hourStart, localDay } from "../local-time";
import type { Filter, Interval, Range } from "../model";
import type { RouteRow } from "../postgres/routes";

/** `today`, `7d`, `30d`, or `YYYY-MM-DD..YYYY-MM-DD`: site-local days, both ends included. */
export type RangeSpec = string;
export const PRESET_RANGES = [
  { spec: "today", label: "Today" },
  { spec: "7d", label: "7 days" },
  { spec: "30d", label: "30 days" },
] as const;
export const DEFAULT_RANGE: RangeSpec = "30d";

const DAY = /^\d{4}-\d{2}-\d{2}$/;
/** A calendar day: `2026-02-30` matches the shape but rolls over to March. */
const isDay = (day: string) => DAY.test(day) && localDay("UTC", dayStart("UTC", day)) === day;

export const customRange = (from: string, to: string): RangeSpec => `${from}..${to}`;

/** The custom range's first and last day, or null for a preset or a malformed spec. */
export function customDays(spec: RangeSpec) {
  const [from, to, ...rest] = spec.split("..");
  if (from === undefined || to === undefined || rest.length > 0 || !isDay(from) || !isDay(to) || from > to) return null;
  return { from, to };
}

export const isRangeSpec = (spec: string) => PRESET_RANGES.some((preset) => preset.spec === spec) || customDays(spec) !== null;

const QUARTER_HOUR_MS = 900_000;

/** The next site-local hour boundary after `now`'s hour. Across a half-hour offset change that boundary is 30 minutes away, not an hour. */
function nextHourStart(timezone: string, now: number) {
  const start = hourStart(timezone, new Date(now)).getTime();
  for (let at = start + QUARTER_HOUR_MS; ; at += QUARTER_HOUR_MS) {
    const next = hourStart(timezone, new Date(at));
    if (next.getTime() > start) return next;
  }
}

export type ResolvedRange = { readonly range: Range; readonly interval: Interval };

/**
 * The instants a spec covers in the site's zone, through the end of the current hour. A day range reads hourly; anything
 * longer reads daily.
 */
export function resolveRange(spec: RangeSpec, timezone: string, now: number): ResolvedRange {
  const end = nextHourStart(timezone, now);
  const today = localDay(timezone, end.getTime() - 1);
  const custom = customDays(spec);
  if (custom !== null) {
    const start = dayStart(timezone, custom.from);
    if (start >= end) return resolveRange(DEFAULT_RANGE, timezone, now);
    const last = dayStart(timezone, custom.to, 1);
    return { range: { start, end: last < end ? last : end }, interval: custom.from === custom.to ? "hour" : "day" };
  }
  const days = spec === "today" ? 1 : spec === "7d" ? 7 : 30;
  return { range: { start: dayStart(timezone, addDays(timezone, today, 1 - days)), end }, interval: days === 1 ? "hour" : "day" };
}

export const ANALYTICS_VIEWS = ["web", "api", "errors"] as const;
export type AnalyticsViewKind = (typeof ANALYTICS_VIEWS)[number];
export const isAnalyticsView = (view: string): view is AnalyticsViewKind => ANALYTICS_VIEWS.some((known) => known === view);

/** How far back the API and Errors views read. */
export const REQUEST_WINDOWS = [
  { spec: "1h", hours: 1 },
  { spec: "6h", hours: 6 },
  { spec: "24h", hours: 24 },
  { spec: "7d", hours: 168 },
  { spec: "30d", hours: 720 },
] as const;
export type RequestWindow = (typeof REQUEST_WINDOWS)[number]["spec"];
export const DEFAULT_WINDOW: RequestWindow = "24h";

export const isRequestWindow = (spec: string): spec is RequestWindow => REQUEST_WINDOWS.some((window) => window.spec === spec);

/**
 * At least the last `window` of requests, in whole site-local hours through the end of the current one: at 10:05, `1h`
 * reads 09:00 to 11:00.
 */
export function resolveWindow(window: RequestWindow, timezone: string, now: number): Range {
  const hours = REQUEST_WINDOWS.find((option) => option.spec === window)?.hours ?? 24;
  return { start: hourStart(timezone, new Date(now - hours * HOUR_MS)), end: nextHourStart(timezone, now) };
}

export type RouteSortKey = "route" | "requests" | "errorRate" | "p50" | "p95" | "p99";
export type RouteSort = { readonly key: RouteSortKey; readonly descending: boolean };
export const DEFAULT_ROUTE_SORT: RouteSort = { key: "requests", descending: true };

/** Routes in the asked order; ties keep the busier route first, then the name. */
export function sortRoutes<R extends RouteRow>(routes: ReadonlyArray<R>, sort: RouteSort): R[] {
  const { key } = sort;
  const value = (row: R) => key === "route" ? 0 : key === "errorRate" ? (row.requests === 0 ? 0 : row.errors / row.requests) : row[key];
  const sign = sort.descending ? -1 : 1;
  return [...routes].sort((a, b) => {
    const order = key === "route" ? a.route.localeCompare(b.route) : value(a) - value(b);
    return order * sign || b.requests - a.requests || a.route.localeCompare(b.route);
  });
}

/** Why a day's numbers are short: some visits could not be counted, or the day is today. */
export type PartialReason = "no-salt" | "today";
export type PartialDay = { readonly day: string; readonly reason: PartialReason };

/** What Cloud knows about how complete each day is. */
export type DayEvidence = {
  readonly today: string;
  /** Days with pageviews a Machine could not tie to a visitor, for want of the day's salt. */
  readonly noSalt: ReadonlyArray<string>;
};

/** Days in `days` with uncounted visitors, then today. */
export function partialDays(days: ReadonlyArray<string>, evidence: DayEvidence): PartialDay[] {
  const noSalt = new Set(evidence.noSalt);
  return days.flatMap((day): PartialDay[] =>
    noSalt.has(day) ? [{ day, reason: "no-salt" }] : day === evidence.today ? [{ day, reason: "today" }] : []);
}

export type AnalyticsPlan = "cloud" | "self-host-on" | "self-host-off";
export type ClosedGate = "self-host-off" | "site-off";

/**
 * What the page can show. Cloud gives analytics to everyone, a self-hosted Cloud runs it when its operator
 * turns it on, and an Environment counts nothing until someone turns it on there.
 */
export function analyticsGate<S extends { readonly enabled: boolean }>(plan: AnalyticsPlan, site: S | undefined):
  { readonly gate: "on"; readonly site: S } | { readonly gate: ClosedGate } {
  if (plan === "self-host-off") return { gate: plan };
  return site?.enabled ? { gate: "on", site } : { gate: "site-off" };
}

/** `?page=eq:/pricing` in the URL: one page filter at most. */
export function parsePageFilter(text: string | undefined): Filter | null {
  const match = text === undefined ? null : /^(eq|neq):(\/.*)$/.exec(text);
  if (match === null) return null;
  return { key: "page", op: match[1] === "neq" ? "neq" : "eq", value: match[2] ?? "/" };
}

export const pageFilterParam = (filter: Filter) => `${filter.op}:${filter.value}`;

/** A goal's path pattern: a path, with `*` matching any run of characters. */
export const isPathPattern = (pattern: string) => pattern.startsWith("/") && pattern.length <= 512 && !/\s/.test(pattern);

const compact = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });
const whole = new Intl.NumberFormat("en-US");

export const formatCount = (value: number) => value < 10_000 ? whole.format(value) : compact.format(value);
export const formatPercent = (value: number) => `${Math.round(value)}%`;

export function formatDuration(seconds: number) {
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${String(Math.round(seconds % 60)).padStart(2, "0")}s`;
}

export const formatMs = (ms: number) => ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)}s`;

/** A share of requests: tenths below 10%, so a rare failure never reads as none. */
export function formatRate(part: number, whole: number) {
  if (whole === 0 || part === 0) return "0%";
  const percent = (part / whole) * 100;
  return percent < 10 ? `${percent.toFixed(1)}%` : `${Math.round(percent)}%`;
}

/** The share of the range's visitors who reached the goal, or null before anyone visited. */
export const goalRate = (goalVisitors: number, visitors: number) => visitors === 0 ? null : (goalVisitors / visitors) * 100;

export const viewsPerVisit = (pageviews: number, visits: number) => visits === 0 ? 0 : Math.round((pageviews / visits) * 10) / 10;

const EnvironmentScope = { organizationSlug: Schema.NonEmptyString, environmentId: Schema.NonEmptyString };

export const ReadAnalyticsViewInput = Schema.Struct({ ...EnvironmentScope, range: Schema.String, page: Schema.optional(Schema.String) });
export type ReadAnalyticsViewInput = typeof ReadAnalyticsViewInput.Type;

export const ReadRequestViewInput = Schema.Struct({
  ...EnvironmentScope,
  window: Schema.String,
  service: Schema.optional(Schema.NonEmptyString),
});
export type ReadRequestViewInput = typeof ReadRequestViewInput.Type;

export const TurnOnAnalyticsInput = Schema.Struct({ ...EnvironmentScope, timezone: Schema.NonEmptyString });
export type TurnOnAnalyticsInput = typeof TurnOnAnalyticsInput.Type;

export const CreateGoalInput = Schema.Struct({ ...EnvironmentScope, name: Schema.NonEmptyString.check(Schema.isMaxLength(80)), pathPattern: Schema.NonEmptyString });
export type CreateGoalInput = typeof CreateGoalInput.Type;

export const DeleteGoalInput = Schema.Struct({ ...EnvironmentScope, id: Schema.NonEmptyString });
export type DeleteGoalInput = typeof DeleteGoalInput.Type;
