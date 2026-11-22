import { DAY_MS, HOUR_MS, MINUTE_MS, type RangeWindow } from "#/lib/time-range";
import { retentionCutoff } from "./retention";

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
