import { Schema } from "effect";
import { addDays, dayStart, HOUR_MS, hourStart, localDay } from "../local-time";
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

export const customRange = (from: string, to: string): RangeSpec => `${from}..${to}`;

/** The custom range's first and last day, or null for a preset or a malformed spec. */
export function customDays(spec: RangeSpec) {
  const [from, to, ...rest] = spec.split("..");
  if (from === undefined || to === undefined || rest.length > 0 || !DAY.test(from) || !DAY.test(to) || from > to) return null;
  return { from, to };
}

export const isRangeSpec = (spec: string) => PRESET_RANGES.some((preset) => preset.spec === spec) || customDays(spec) !== null;

export type ResolvedRange = { readonly range: Range; readonly interval: Interval };

/**
 * The instants a spec covers in the site's zone, through the end of the current hour: the same bounds as
 * `pnpm analytics:query --range Nd`. A day range reads hourly; anything longer reads daily.
 */
export function resolveRange(spec: RangeSpec, timezone: string, now: number): ResolvedRange {
  const end = new Date(hourStart(timezone, new Date(now)).getTime() + HOUR_MS);
  const today = localDay(timezone, end.getTime() - 1);
  const custom = customDays(spec);
  if (custom !== null) {
    const start = dayStart(timezone, custom.from);
    const last = dayStart(timezone, custom.to, 1);
    const range = { start: start < end ? start : new Date(end.getTime() - HOUR_MS), end: last < end ? last : end };
    return { range, interval: custom.from === custom.to ? "hour" : "day" };
  }
  const days = spec === "today" ? 1 : spec === "7d" ? 7 : 30;
  return { range: { start: dayStart(timezone, addDays(timezone, today, 1 - days)), end }, interval: days === 1 ? "hour" : "day" };
}

/** Why a day's numbers are short: some visits could not be counted, or the day is still arriving. */
export type PartialReason = "gap" | "today";
export type PartialDay = { readonly day: string; readonly reason: PartialReason };

/** Days in `days` with uncounted visits, then the site's current day, which pageviews still reach late. */
export function partialDays(days: ReadonlyArray<string>, gaps: ReadonlyArray<{ day: string; count: number }>, today: string): PartialDay[] {
  const gapped = new Set(gaps.filter((gap) => gap.count > 0).map((gap) => gap.day));
  return days.flatMap((day): PartialDay[] => gapped.has(day) ? [{ day, reason: "gap" }] : day === today ? [{ day, reason: "today" }] : []);
}

export type AnalyticsPlan = "paid" | "unpaid" | "self-host-on" | "self-host-off";
export type ClosedGate = "unpaid" | "self-host-off" | "site-off";

/**
 * What the page can show. Cloud sells analytics with the paid plan, a self-hosted Cloud runs it when its operator
 * turns it on, and an Environment counts nothing until someone turns it on there.
 */
export function analyticsGate<S extends { readonly enabled: boolean }>(plan: AnalyticsPlan, site: S | undefined):
  { readonly gate: "on"; readonly site: S } | { readonly gate: ClosedGate } {
  if (plan === "unpaid" || plan === "self-host-off") return { gate: plan };
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

export const CreateGoalInput = Schema.Struct({ ...EnvironmentScope, name: Schema.NonEmptyString, pathPattern: Schema.NonEmptyString });
export type CreateGoalInput = typeof CreateGoalInput.Type;

export const DeleteGoalInput = Schema.Struct({ ...EnvironmentScope, id: Schema.NonEmptyString });
export type DeleteGoalInput = typeof DeleteGoalInput.Type;
