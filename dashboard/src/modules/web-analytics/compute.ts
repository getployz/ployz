import { dayStart, hourBuckets, localDays } from "./local-time";
import type { BucketRow, EntryRow, GoalRow, Interval, Range, SessionRow, Totals, ValueRow } from "./model";
import { ZERO_TOTALS } from "./model";

export type Overview = Totals & { readonly bounceRate: number; readonly avgDuration: number };
export type Bucket = BucketRow & { readonly bounceRate: number; readonly avgDuration: number };
export type SessionResult = SessionRow & { readonly bounceRate: number; readonly avgDuration: number };
export type EntryResult = EntryRow & { readonly bounceRate: number; readonly avgDuration: number };

/** Bounce rate in percent to one decimal; average session length in whole seconds. Both 0 without visits. */
export function rates(row: { readonly visits: number; readonly bounces: number; readonly totalDuration: number }) {
  if (row.visits === 0) return { bounceRate: 0, avgDuration: 0 };
  return {
    bounceRate: Math.round((row.bounces / row.visits) * 1000) / 10,
    avgDuration: Math.round(row.totalDuration / row.visits),
  };
}

export function sumTotals(rows: ReadonlyArray<Totals>): Totals {
  const sum = { ...ZERO_TOTALS };
  for (const row of rows) {
    sum.pageviews += row.pageviews;
    sum.visitors += row.visitors;
    sum.visits += row.visits;
    sum.bounces += row.bounces;
    sum.totalDuration += row.totalDuration;
  }
  return sum;
}

export const overview = (rows: ReadonlyArray<Totals>): Overview => {
  const totals = sumTotals(rows);
  return { ...totals, ...rates(totals) };
};

/** Sum grouped rows that share a value, as when a rollup part and a raw part both hold the same page. */
export function mergeValues<Row extends { readonly value: string }>(rows: ReadonlyArray<Row>, add: (a: Row, b: Row) => Row) {
  const byValue = new Map<string, Row>();
  for (const row of rows) {
    const seen = byValue.get(row.value);
    byValue.set(row.value, seen === undefined ? row : add(seen, row));
  }
  return [...byValue.values()];
}

const addValue = (a: ValueRow, b: ValueRow): ValueRow => ({ value: a.value, pageviews: a.pageviews + b.pageviews, visitors: a.visitors + b.visitors });
const addSession = (a: SessionRow, b: SessionRow): SessionRow => ({
  value: a.value, visits: a.visits + b.visits, bounces: a.bounces + b.bounces, totalDuration: a.totalDuration + b.totalDuration,
});
const addEntry = (a: EntryRow, b: EntryRow): EntryRow => ({ ...addSession(a, b), visitors: a.visitors + b.visitors });

const byCountThenValue = <Row extends { readonly value: string }>(count: (row: Row) => number) =>
  (a: Row, b: Row) => count(b) - count(a) || (a.value < b.value ? -1 : a.value > b.value ? 1 : 0);

export const breakdown = (rows: ReadonlyArray<ValueRow>, limit: number): ValueRow[] =>
  mergeValues(rows, addValue).sort(byCountThenValue<ValueRow>((row) => row.visitors)).slice(0, limit);

export const sessions = (rows: ReadonlyArray<SessionRow>, limit: number): SessionResult[] =>
  mergeValues(rows, addSession).sort(byCountThenValue<SessionRow>((row) => row.visits)).slice(0, limit)
    .map((row) => ({ ...row, ...rates(row) }));

export const entryPages = (rows: ReadonlyArray<EntryRow>, limit: number): EntryResult[] =>
  mergeValues(rows, addEntry).sort(byCountThenValue<EntryRow>((row) => row.visitors)).slice(0, limit)
    .map((row) => ({ ...row, ...rates(row) }));

export const goalConversions = (rows: ReadonlyArray<GoalRow>): GoalRow[] =>
  [...rows].sort((a, b) => b.visitors - a.visitors || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));

/** One bucket per site-local hour or day in the range, zero where nothing happened. A DST day has 23 or 25 hours. */
export function timeseries(rows: ReadonlyArray<BucketRow>, range: Range, timezone: string, interval: Interval): Bucket[] {
  const starts = interval === "hour"
    ? hourBuckets(range.start, range.end)
    : localDays(timezone, range.start, range.end).map((day) => dayStart(timezone, day));
  const byBucket = new Map<number, Totals>();
  for (const row of rows) {
    const at = row.bucket.getTime();
    byBucket.set(at, sumTotals([byBucket.get(at) ?? ZERO_TOTALS, row]));
  }
  return starts.map((bucket) => {
    const totals = byBucket.get(bucket.getTime()) ?? ZERO_TOTALS;
    return { bucket, ...totals, ...rates(totals) };
  });
}
