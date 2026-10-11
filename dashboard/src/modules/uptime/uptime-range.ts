import { TZDate, tzOffset } from "@date-fns/tz";
import * as chrono from "chrono-node";
import type { Component } from "chrono-node";
import { Schema } from "effect";
import { retentionCutoff } from "./retention";

const SECOND_MS = 1000;
const MINUTE_MS = 60 * SECOND_MS;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const UNIT_MS = { s: SECOND_MS, m: MINUTE_MS, h: HOUR_MS, d: DAY_MS, w: 7 * DAY_MS } as const;
type Unit = keyof typeof UNIT_MS;

const AGO = /^([1-9]\d{0,5})([smhdw])$/u;
const INSTANT = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/u;

/**
 * One end of a range as the URL, the query key and the server read it: "now", an offset back from now ("15m"), or a
 * UTC instant ("2026-10-11T14:00:00.000Z"). Relative edges resolve against the reader's clock, so the server and the
 * client never need to agree on a time zone; only `parseEdge` does, and it runs where the person types.
 */
export const RangeEdge = Schema.String.check(Schema.makeFilter((edge: string) =>
  edge === "now" || AGO.test(edge) || (INSTANT.test(edge) && !Number.isNaN(Date.parse(edge)) && instantEdge(Date.parse(edge)) === edge)));

export type UptimeRange = { readonly from: string; readonly to: string };

export const QUICK_RANGES = [
  { from: "5m", label: "5 min" },
  { from: "15m", label: "15 min" },
  { from: "30m", label: "30 min" },
  { from: "1h", label: "1 hour" },
  { from: "3h", label: "3 hours" },
  { from: "6h", label: "6 hours" },
  { from: "1d", label: "1 day" },
  { from: "7d", label: "7 days" },
  { from: "30d", label: "30 days" },
] as const;

export const DEFAULT_RANGE: UptimeRange = { from: "1h", to: "now" };

export const instantEdge = (ms: number) => new Date(ms).toISOString();

export function edgeMs(edge: string, now: number): number {
  if (edge === "now") return now;
  const ago = AGO.exec(edge);
  // SAFETY: AGO's second group only matches a key of UNIT_MS.
  return ago ? now - Number(ago[1]) * UNIT_MS[ago[2] as Unit] : Date.parse(edge);
}

export type RangeWindow = { readonly from: number; readonly to: number };

/** `range` at `now`: never past now, never backwards, never before 1970, so a far offset still reads as an instant the database holds. */
export function resolveRange(range: UptimeRange, now: number): RangeWindow {
  const to = Math.max(0, Math.min(edgeMs(range.to, now), now));
  return { from: Math.max(0, Math.min(edgeMs(range.from, now), to)), to };
}

/** Checks are sparse, so the strip and uptime always cover at least the last 30 minutes of a range. */
export const CHECK_MIN_MS = 30 * MINUTE_MS;

export const checkWindow = (window: RangeWindow): RangeWindow =>
  ({ from: Math.min(window.from, window.to - CHECK_MIN_MS), to: window.to });

/** A check window this short shows one bar per result, the newest `MAX_CHECK_BARS` of them. */
export const PER_CHECK_MS = 6 * HOUR_MS;
export const MAX_CHECK_BARS = 120;

export const perCheck = (window: RangeWindow) => window.to - window.from <= PER_CHECK_MS;

/** Where a window's latency points come from: raw results up to a day back within raw retention, then the hourly rollup. */
export type PointSource = "raw" | "hourly" | "sixHourly";

export function pointSource(window: RangeWindow, now: number): PointSource {
  const length = window.to - window.from;
  if (length <= DAY_MS && window.from >= retentionCutoff("raw", new Date(now)).getTime()) return "raw";
  return length <= 7 * DAY_MS ? "hourly" : "sixHourly";
}

/** The coarsest whole unit an offset back from now fits, as an edge, or null when none does in six digits. */
function agoEdge(ms: number) {
  const unit = (["w", "d", "h", "m", "s"] satisfies Unit[]).find((candidate) => ms % UNIT_MS[candidate] === 0);
  return unit && ms / UNIT_MS[unit] < 1_000_000 ? `${ms / UNIT_MS[unit]}${unit}` : null;
}

/**
 * What a person typed into From or Until, as an edge, or null when it names no time. chrono-node reads the text: "now",
 * "15m ago", "2 hours ago", "yesterday", "2023-12-25 14:30", ISO instants. "15m" reads as "15m ago". An offset back
 * from now stays relative; a day without a time is its midnight; wall clock times are in `timeZone`.
 */
export function parseEdge(input: string, timeZone: string, now: number): string | null {
  const trimmed = input.trim().toLowerCase();
  const text = /^\d+\s*[a-z]+$/u.test(trimmed) ? `${trimmed} ago` : trimmed;
  const reference = { instant: new Date(now), timezone: tzOffset(timeZone, new Date(now)) };
  const [result, ...rest] = chrono.parse(text, reference, { forwardDate: false });
  if (!result || rest.length > 0 || result.end || result.index !== 0 || result.text.length !== text.length) return null;
  const tags = result.tags();
  if (tags.has("casualReference/now")) return "now";
  if (tags.has("result/relativeDate")) {
    const back = now - result.start.date().getTime();
    if (back === 0) return "now";
    if (back > 0) return agoEdge(back) ?? instantEdge(now - back);
  }
  if (result.start.isCertain("timezoneOffset")) return instantEdge(result.start.date().getTime());
  const field = (component: Component) => result.start.get(component) ?? 0;
  const timed = result.start.isCertain("hour");
  const wall = new TZDate(field("year"), field("month") - 1, field("day"),
    timed ? field("hour") : 0, timed ? field("minute") : 0, timed ? field("second") : 0, timeZone);
  return instantEdge(wall.getTime());
}

/** An edge as From and Until show it, in a form `parseEdge` reads back: "now", "15m ago", "2026-10-11 14:00". */
export function edgeText(edge: string, timeZone: string): string {
  if (edge === "now" || AGO.test(edge)) return edge === "now" ? "now" : `${edge} ago`;
  const date = new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone }).format(Date.parse(edge));
  const time = new Intl.DateTimeFormat("en-GB", { hourCycle: "h23", hour: "2-digit", minute: "2-digit", timeZone }).format(Date.parse(edge));
  return `${date} ${time}`;
}

/** The picker's button: "Last 15 min", "Last 2h", "Oct 10, 14:00 – now", or with years when its edges fall in different ones. */
export function rangeLabel(range: UptimeRange, timeZone: string, now: number): string {
  if (range.to === "now") {
    const quick = QUICK_RANGES.find((candidate) => candidate.from === range.from);
    if (quick) return `Last ${quick.label}`;
    if (AGO.test(range.from)) return `Last ${range.from}`;
  }
  const year = (edge: string) => new Intl.DateTimeFormat("en-US", { year: "numeric", timeZone }).format(edgeMs(edge, now));
  const parts: Intl.DateTimeFormatOptions = { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone };
  if (year(range.from) !== year(range.to)) parts.year = "numeric";
  const short = new Intl.DateTimeFormat("en-US", parts);
  const end = (edge: string) => edge === "now" ? "now" : AGO.test(edge) ? `${edge} ago` : short.format(Date.parse(edge));
  return `${end(range.from)} – ${end(range.to)}`;
}
