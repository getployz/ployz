import { Schema } from "effect";
import { addDays, dayStart, hourStart, localDay } from "../local-time";
import type { Filter, Interval, Range } from "../model";

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
 * The instants a spec covers in the site's zone, through the end of the current hour: the same bounds as
 * `pnpm analytics:query --range Nd`. A day range reads hourly; anything longer reads daily.
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

/**
 * Why a day's numbers are short: some visits could not be counted, the day is today, or a past day's visits may
 * still be arriving late.
 */
export type PartialReason = "gap" | "today" | "late";
export type PartialDay = { readonly day: string; readonly reason: PartialReason };

/** What Cloud knows about how complete each day is. */
export type DayEvidence = {
  readonly gaps: ReadonlyArray<{ readonly day: string; readonly count: number }>;
  readonly today: string;
  /** The last day the rollup closed, which it does only once a Machine's late pageviews for it stop being likely. */
  readonly closedThrough: string;
  /** Closed days that pageviews reached after closing, not yet rebuilt. */
  readonly dirty: ReadonlyArray<string>;
};

/** Days in `days` with uncounted visits, then today, then past days whose pageviews may still reach Cloud. */
export function partialDays(days: ReadonlyArray<string>, evidence: DayEvidence): PartialDay[] {
  const gapped = new Set(evidence.gaps.filter((gap) => gap.count > 0).map((gap) => gap.day));
  const dirty = new Set(evidence.dirty);
  const reason = (day: string): PartialReason | undefined =>
    gapped.has(day) ? "gap"
    : day === evidence.today ? "today"
    : day > evidence.closedThrough || dirty.has(day) ? "late"
    : undefined;
  return days.flatMap((day): PartialDay[] => {
    const why = reason(day);
    return why === undefined ? [] : [{ day, reason: why }];
  });
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

/** The share of the range's visitors who reached the goal, or null before anyone visited. */
export const goalRate = (goalVisitors: number, visitors: number) => visitors === 0 ? null : (goalVisitors / visitors) * 100;

export const viewsPerVisit = (pageviews: number, visits: number) => visits === 0 ? 0 : Math.round((pageviews / visits) * 10) / 10;

const EnvironmentScope = { organizationSlug: Schema.NonEmptyString, environmentId: Schema.NonEmptyString };

export const ReadAnalyticsViewInput = Schema.Struct({ ...EnvironmentScope, range: Schema.String, page: Schema.optional(Schema.String) });
export type ReadAnalyticsViewInput = typeof ReadAnalyticsViewInput.Type;

export const TurnOnAnalyticsInput = Schema.Struct({ ...EnvironmentScope, timezone: Schema.NonEmptyString });
export type TurnOnAnalyticsInput = typeof TurnOnAnalyticsInput.Type;

export const CreateGoalInput = Schema.Struct({ ...EnvironmentScope, name: Schema.NonEmptyString.check(Schema.isMaxLength(80)), pathPattern: Schema.NonEmptyString });
export type CreateGoalInput = typeof CreateGoalInput.Type;

export const DeleteGoalInput = Schema.Struct({ ...EnvironmentScope, id: Schema.NonEmptyString });
export type DeleteGoalInput = typeof DeleteGoalInput.Type;
