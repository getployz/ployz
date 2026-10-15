import { TZDate } from "@date-fns/tz";
import { LONGEST_RETENTION_MONTHS } from "./model";

export const HOUR_MS = 3_600_000;
export const SESSION_GAP_SECONDS = 1800;

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

export function hourStart(timezone: string, instant: Date) {
  const local = new TZDate(instant.getTime(), timezone);
  return new Date(instant.getTime() - (local.getMinutes() * 60 + local.getSeconds()) * 1000 - local.getMilliseconds());
}

export const isHourStart = (timezone: string, instant: Date) => hourStart(timezone, instant).getTime() === instant.getTime();

export function retentionFloor(timezone: string, now: number, months: number) {
  const today = new TZDate(now, timezone);
  const first = new Date(Date.UTC(today.getFullYear(), today.getMonth() - months, 1));
  const lastDate = new Date(Date.UTC(first.getUTCFullYear(), first.getUTCMonth() + 1, 0)).getUTCDate();
  return new Date(new TZDate(first.getUTCFullYear(), first.getUTCMonth(), Math.min(today.getDate(), lastDate), timezone).getTime());
}

/** Raw and aggregated rows strictly before this instant are outside every site's retention window. */
export function retentionCutoff(now: number) {
  const floor = retentionFloor("UTC", now, LONGEST_RETENTION_MONTHS);
  return new Date(floor.getTime() - 24 * HOUR_MS);
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
