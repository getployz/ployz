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
export const RangeEdge = Schema.String.check(Schema.makeFilter((edge: string) => edge === "now" || AGO.test(edge) || INSTANT.test(edge)));

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

/** `range` at `now`: never past now, never backwards. */
export function resolveRange(range: UptimeRange, now: number): RangeWindow {
  const to = Math.min(edgeMs(range.to, now), now);
  return { from: Math.min(edgeMs(range.from, now), to), to };
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

const UNIT_WORDS = [
  ["s", ["s", "sec", "secs", "second", "seconds"]],
  ["m", ["m", "min", "mins", "minute", "minutes"]],
  ["h", ["h", "hr", "hrs", "hour", "hours"]],
  ["d", ["d", "day", "days"]],
  ["w", ["w", "wk", "wks", "week", "weeks"]],
] satisfies readonly (readonly [Unit, readonly string[]])[];

/** How far `timeZone` is ahead of UTC at `at`. */
function zoneOffset(at: number, timeZone: string) {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone, hourCycle: "h23", year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric",
  }).formatToParts(at);
  const field = (type: Intl.DateTimeFormatPartTypes) => Number(parts.find((part) => part.type === type)?.value);
  const wall = Date.UTC(field("year"), field("month") - 1, field("day"), field("hour"), field("minute"), field("second"));
  return wall - Math.floor(at / SECOND_MS) * SECOND_MS;
}

/**
 * The instant a wall-clock time in `timeZone` names, given as UTC fields. A time the clocks fall back over names its
 * first instant; a time they spring forward over lands as far past the jump as it was past the last valid time.
 */
function zonedInstant(wall: number, timeZone: string) {
  const offsets = [zoneOffset(wall - DAY_MS / 2, timeZone), zoneOffset(wall + DAY_MS / 2, timeZone)];
  const exact = offsets.map((offset) => wall - offset).filter((at) => at + zoneOffset(at, timeZone) === wall);
  return exact.length > 0 ? Math.min(...exact) : wall - (offsets[0] ?? 0);
}

/** Midnight of the day `at` falls on in `timeZone`, as a wall time in UTC fields. */
function wallDay(at: number, timeZone: string) {
  const wall = at + zoneOffset(at, timeZone);
  return wall - (wall % DAY_MS);
}

/**
 * What a person typed into From or Until, as an edge, or null when it names no time. Accepts "now", "15m", "15m ago",
 * "2 hours ago", "today", "yesterday", "2023-12-25", "2023-12-25 14:30" in `timeZone`, and ISO instants with a zone.
 */
export function parseEdge(input: string, timeZone: string, now: number): string | null {
  const text = input.trim().toLowerCase();
  if (text === "now") return "now";
  if (text === "today" || text === "yesterday") {
    const day = wallDay(now, timeZone) - (text === "yesterday" ? DAY_MS : 0);
    return instantEdge(zonedInstant(day, timeZone));
  }
  const ago = /^(\d{1,6})\s*([a-z]+)(?:\s+ago)?$/u.exec(text);
  if (ago) {
    const unit = UNIT_WORDS.find(([, words]) => words.includes(ago[2] ?? ""))?.[0];
    if (unit === undefined) return null;
    return Number(ago[1]) === 0 ? "now" : `${Number(ago[1])}${unit}`;
  }
  const wall = /^(\d{4})-(\d{2})-(\d{2})(?:[ t](\d{1,2}):(\d{2})(?::(\d{2}))?)?$/u.exec(text);
  if (wall) {
    const [year, month, day, hour, minute, second] = wall.slice(1).map((field) => Number(field ?? 0));
    const ms = Date.UTC(year ?? 0, (month ?? 1) - 1, day ?? 1, hour ?? 0, minute ?? 0, second ?? 0);
    const back = new Date(ms);
    const valid = back.getUTCFullYear() === year && back.getUTCMonth() + 1 === month && back.getUTCDate() === day
      && (hour ?? 0) < 24 && (minute ?? 0) < 60 && (second ?? 0) < 60;
    return valid ? instantEdge(zonedInstant(ms, timeZone)) : null;
  }
  if (/^\d{4}-\d{2}-\d{2}t\d{2}:\d{2}(:\d{2}(\.\d+)?)?(z|[+-]\d{2}:?\d{2})$/u.test(text)) {
    const ms = Date.parse(text.toUpperCase());
    return Number.isNaN(ms) ? null : instantEdge(ms);
  }
  return null;
}

/** An edge as From and Until show it, in a form `parseEdge` reads back: "now", "15m ago", "2026-10-11 14:00". */
export function edgeText(edge: string, timeZone: string): string {
  if (edge === "now" || AGO.test(edge)) return edge === "now" ? "now" : `${edge} ago`;
  const date = new Intl.DateTimeFormat("en-CA", { year: "numeric", month: "2-digit", day: "2-digit", timeZone }).format(Date.parse(edge));
  const time = new Intl.DateTimeFormat("en-GB", { hourCycle: "h23", hour: "2-digit", minute: "2-digit", timeZone }).format(Date.parse(edge));
  return `${date} ${time}`;
}

/** The picker's button: "Last 15 min", "Last 2h", or "Oct 10, 14:00 – now". */
export function rangeLabel(range: UptimeRange, timeZone: string): string {
  if (range.to === "now") {
    const quick = QUICK_RANGES.find((candidate) => candidate.from === range.from);
    if (quick) return `Last ${quick.label}`;
    if (AGO.test(range.from)) return `Last ${range.from}`;
  }
  const short = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone });
  const end = (edge: string) => edge === "now" ? "now" : AGO.test(edge) ? `${edge} ago` : short.format(Date.parse(edge));
  return `${end(range.from)} – ${end(range.to)}`;
}
