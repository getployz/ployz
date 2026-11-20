import type { Gap } from "./metrics";

/** What one series said in one bucket: a counter's last value, or a gauge's average. `at` is the bucket start in Unix milliseconds. */
export type SeriesSample = { readonly series: string; readonly at: number; readonly value: number };

export type SeriesWindow = { readonly from: number; readonly to: number };

const SECOND_MS = 1000;
/** Both collectors sample every 10 seconds, so no bucket is narrower. */
const MIN_STEP_MS = 10 * SECOND_MS;
const TARGET_BUCKETS = 300;
const MAX_BUCKETS = 4000;

/** The bucket width for `window`: about 300 buckets, whole multiples of the sampling period, never more than 4,000. */
export function stepFor(window: SeriesWindow): number {
  const wanted = Math.ceil((window.to - window.from) / TARGET_BUCKETS / MIN_STEP_MS) * MIN_STEP_MS;
  const floor = Math.ceil((window.to - window.from) / MAX_BUCKETS / MIN_STEP_MS) * MIN_STEP_MS;
  return Math.max(MIN_STEP_MS, wanted, floor);
}

const bySeriesAndTime = (samples: readonly SeriesSample[]) => {
  const series = new Map<string, SeriesSample[]>();
  for (const sample of samples) {
    const own = series.get(sample.series);
    if (own === undefined) series.set(sample.series, [sample]);
    else own.push(sample);
  }
  for (const own of series.values()) own.sort((left, right) => left.at - right.at);
  return series;
};

/** One counter's growth between two of its consecutive buckets. */
export type Delta = { readonly series: string; readonly at: number; readonly delta: number; readonly seconds: number };

/**
 * Each series's growth since its previous bucket. A series's first bucket has no predecessor and yields none. A value
 * lower than its predecessor is a counter that restarted, so it counts as no growth, never as negative growth.
 */
export function counterDeltas(samples: readonly SeriesSample[]): Delta[] {
  const deltas: Delta[] = [];
  for (const [series, own] of bySeriesAndTime(samples)) {
    for (let index = 1; index < own.length; index++) {
      const previous = own[index - 1];
      const current = own[index];
      if (previous === undefined || current === undefined) continue;
      deltas.push({ series, at: current.at, delta: Math.max(0, current.value - previous.value), seconds: (current.at - previous.at) / SECOND_MS });
    }
  }
  return deltas;
}

/** Per-second growth of every counter in `samples` added together, by bucket. */
export function counterRate(samples: readonly SeriesSample[]): Map<number, number> {
  const rates = new Map<number, number>();
  for (const { at, delta, seconds } of counterDeltas(samples)) rates.set(at, (rates.get(at) ?? 0) + delta / seconds);
  return rates;
}

/** Every gauge in `samples` added together, by bucket. */
export function gaugeSum(samples: readonly SeriesSample[]): Map<number, number> {
  const sums = new Map<number, number>();
  for (const { at, value } of samples) sums.set(at, (sums.get(at) ?? 0) + value);
  return sums;
}

/** The share of counter growth in `busy` out of that in `busy` and `rest`, by bucket. A bucket whose counters did not grow has none. */
export function counterShare(busy: readonly SeriesSample[], rest: readonly SeriesSample[]): Map<number, number> {
  const parts = new Map<number, { busy: number; all: number }>();
  const add = (deltas: readonly Delta[], isBusy: boolean) => {
    for (const { at, delta } of deltas) {
      const part = parts.get(at) ?? { busy: 0, all: 0 };
      parts.set(at, { busy: part.busy + (isBusy ? delta : 0), all: part.all + delta });
    }
  };
  add(counterDeltas(busy), true);
  add(counterDeltas(rest), false);
  const shares = new Map<number, number>();
  for (const [at, { busy: used, all }] of parts) if (all > 0) shares.set(at, Math.min(1, used / all));
  return shares;
}

/**
 * The spans inside the window that no bucket covers, up to the newest bucket. The span after it is the present, not a
 * gap, and `at` is when the read happened.
 */
export function gapsBetween(buckets: ReadonlySet<number>, window: SeriesWindow, stepMs: number, at: number): Gap[] {
  const last = Math.max(...buckets, Number.NEGATIVE_INFINITY);
  if (!Number.isFinite(last)) return [];
  const first = Math.floor(window.from / stepMs) * stepMs;
  const end = Math.min(last, Math.floor(Math.min(window.to, at) / stepMs) * stepMs);
  const gaps: Gap[] = [];
  let open: number | null = null;
  for (let start = first; start <= end; start += stepMs) {
    if (buckets.has(start)) {
      if (open !== null) gaps.push({ from: open, to: start });
      open = null;
    } else if (open === null) {
      open = start;
    }
  }
  return gaps;
}
