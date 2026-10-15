import { TZDate } from "@date-fns/tz";

export const HOUR_MS = 3_600_000;
export const SESSION_GAP_SECONDS = 1800;

/** The site-local calendar day ("YYYY-MM-DD") holding `instant`. */
export function localDay(timezone: string, instant: Date | number) {
  const local = new TZDate(new Date(instant).getTime(), timezone);
  return `${local.getFullYear()}-${pad(local.getMonth() + 1)}-${pad(local.getDate())}`;
}

/** The UTC instant at which local `day` starts, shifted by `offset` days. */
export function dayStart(timezone: string, day: string, offset = 0) {
  const year = Number(day.slice(0, 4));
  const month = Number(day.slice(5, 7));
  const date = Number(day.slice(8, 10));
  return new Date(new TZDate(year, month - 1, date + offset, timezone).getTime());
}

export const dayEnd = (timezone: string, day: string) => dayStart(timezone, day, 1);

export const addDays = (timezone: string, day: string, offset: number) => localDay(timezone, dayStart(timezone, day, offset));

/** The UTC instant at which the site-local hour holding `instant` starts. Exact for half- and quarter-hour offsets too. */
export function hourStart(timezone: string, instant: Date) {
  const local = new TZDate(instant.getTime(), timezone);
  return new Date(instant.getTime() - (local.getMinutes() * 60 + local.getSeconds()) * 1000 - local.getMilliseconds());
}

export const isHourStart = (timezone: string, instant: Date) => hourStart(timezone, instant).getTime() === instant.getTime();

/** Local midnight of local today minus `months` months: the oldest instant a site may still read. */
export function retentionFloor(timezone: string, now: number, months: number) {
  const today = new TZDate(now, timezone);
  return new Date(new TZDate(today.getFullYear(), today.getMonth() - months, today.getDate(), timezone).getTime());
}

/** Every local day with at least one instant in `[start, end)`, oldest first. */
export function localDays(timezone: string, start: Date, end: Date) {
  const days: string[] = [];
  if (start >= end) return days;
  for (let day = localDay(timezone, start); dayStart(timezone, day) < end; day = addDays(timezone, day, 1)) days.push(day);
  return days;
}

/** Start instants of every local hour bucket in `[start, end)`, for a range aligned to hour starts. */
export function hourBuckets(start: Date, end: Date) {
  const buckets: Date[] = [];
  for (let at = start.getTime(); at < end.getTime(); at += HOUR_MS) buckets.push(new Date(at));
  return buckets;
}

const pad = (n: number) => String(n).padStart(2, "0");
