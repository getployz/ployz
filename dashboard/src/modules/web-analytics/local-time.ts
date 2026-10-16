import { TZDate } from "@date-fns/tz";
import { RETENTION_DAYS } from "./model";

export const HOUR_MS = 3_600_000;
export const SESSION_GAP_SECONDS = 1800;
/** How far before a range a visit crossing into it is followed back to find its entry. A longer visit enters at this bound. */
export const ENTRY_LOOKBACK_SECONDS = 86_400;

export function localDay(timezone: string, instant: Date | number) {
  const local = new TZDate(new Date(instant).getTime(), timezone);
  return `${local.getFullYear()}-${pad(local.getMonth() + 1)}-${pad(local.getDate())}`;
}

export function dayStart(timezone: string, day: string, offset = 0) {
  const year = Number(day.slice(0, 4));
  const month = Number(day.slice(5, 7));
  const date = Number(day.slice(8, 10));
  return new Date(new TZDate(year, month - 1, date + offset, timezone).getTime());
}

export const dayEnd = (timezone: string, day: string) => dayStart(timezone, day, 1);

export const addDays = (timezone: string, day: string, offset: number) => localDay(timezone, dayStart(timezone, day, offset));

const MINUTE_MS = 60_000;
const offsetAt = (timezone: string, at: number) => new TZDate(at, timezone).getTimezoneOffset();

/**
 * The first instant of the site-local hour holding `instant`. Stepping back the wall-clock time past the hour lands
 * before an offset change when one falls inside the hour, so the hour then starts at the change: Lord Howe's local
 * 02 on a spring-forward day starts at 02:30, and its repeated 01:30 on a fall-back day starts a bucket of its own.
 */
export function hourStart(timezone: string, instant: Date) {
  const local = new TZDate(instant.getTime(), timezone);
  const back = instant.getTime() - (local.getMinutes() * 60 + local.getSeconds()) * 1000 - local.getMilliseconds();
  const offset = offsetAt(timezone, instant.getTime());
  if (offsetAt(timezone, back) === offset) return new Date(back);
  let at = back + MINUTE_MS;
  while (offsetAt(timezone, at) !== offset) at += MINUTE_MS;
  return new Date(at);
}

export const isHourStart = (timezone: string, instant: Date) => hourStart(timezone, instant).getTime() === instant.getTime();

/** The start of the oldest site-local day kept: today and the 29 days before it, the 30d range. */
export function retentionFloor(timezone: string, now: number) {
  return dayStart(timezone, localDay(timezone, now), 1 - RETENTION_DAYS);
}

/** Rows strictly before this instant are outside every site's retention window. */
export function retentionCutoff(now: number) {
  // No zone's local date runs more than a day behind UTC's, so a day before the UTC floor covers them all.
  return new Date(retentionFloor("UTC", now).getTime() - 24 * HOUR_MS);
}

export function localDays(timezone: string, start: Date, end: Date) {
  const days: string[] = [];
  if (start >= end) return days;
  for (let day = localDay(timezone, start); dayStart(timezone, day) < end; day = addDays(timezone, day, 1)) days.push(day);
  return days;
}

const QUARTER_HOUR_MS = 900_000;

export function hourBuckets(timezone: string, start: Date, end: Date) {
  const buckets: Date[] = [];
  for (let at = start.getTime(); at < end.getTime(); at += QUARTER_HOUR_MS) {
    const bucket = hourStart(timezone, new Date(at));
    if (buckets.at(-1)?.getTime() !== bucket.getTime()) buckets.push(bucket);
  }
  return buckets;
}

const pad = (n: number) => String(n).padStart(2, "0");
