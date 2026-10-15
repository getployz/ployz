import { TZDate } from "@date-fns/tz";

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
  return new Date(new TZDate(today.getFullYear(), today.getMonth() - months, today.getDate(), timezone).getTime());
}

export function localDays(timezone: string, start: Date, end: Date) {
  const days: string[] = [];
  if (start >= end) return days;
  for (let day = localDay(timezone, start); dayStart(timezone, day) < end; day = addDays(timezone, day, 1)) days.push(day);
  return days;
}

export function hourBuckets(start: Date, end: Date) {
  const buckets: Date[] = [];
  for (let at = start.getTime(); at < end.getTime(); at += HOUR_MS) buckets.push(new Date(at));
  return buckets;
}

const pad = (n: number) => String(n).padStart(2, "0");
