import type { ServerMetrics } from "#/modules/metrics/metrics-model";

/** The read that answers what a Server uses now: short enough to stay on the daemon's raw 10 second samples. */
export const NOW_RANGE = { from: "5m", to: "now" } as const;

export type Usage = { readonly used: number; readonly total: number };

export type ServerNow =
  | {
    readonly type: "meters";
    /** Fraction of all CPUs busy, 0 to 1. */
    readonly cpu: number | null;
    readonly memory: Usage | null;
    readonly disk: Usage | null;
  }
  | { readonly type: "needs_upgrade" }
  | { readonly type: "unanswered" }
  | { readonly type: "no_readings" };

export const usageFraction = (usage: Usage) => usage.used / usage.total;

// The daemon samples CPU and memory every 10 seconds and disk every 60; `ployz metrics` calls three samples fresh.
const FRESH_SAMPLES = 3;
const HOST_SAMPLE_MS = 10_000;
export const DISK_SAMPLE_MS = 60_000;

/**
 * The last of `samples` while it still counts as now: within three of its own samples, or three buckets on a broader
 * read, of now. `age` is how long ago the read landed; now is the read's end moved on by it, which keeps the browser's
 * clock out of the comparison.
 */
export function freshLast<S extends { readonly at: number }>(
  samples: readonly S[],
  read: { readonly window: { readonly to: number }; readonly stepMs: number | null },
  age: number,
  sampleMs = HOST_SAMPLE_MS,
): S | undefined {
  const last = samples.at(-1);
  return last !== undefined && last.at >= read.window.to + age - FRESH_SAMPLES * Math.max(read.stepMs ?? 0, sampleMs) ? last : undefined;
}

const usage = (used: number, total: number): Usage | null => total > 0 ? { used, total } : null;

/** What a Server uses now, from a read that landed `age` ms ago, leaving out any reading too old to call current. */
export function serverNow(metrics: ServerMetrics, age: number): ServerNow {
  switch (metrics.availability.type) {
    case "needs_upgrade":
      return { type: "needs_upgrade" };
    case "unanswered":
    case "offline":
      return { type: "unanswered" };
    case "no_readings":
      return { type: "no_readings" };
    case "normal":
    case "mixed": {
      const cpu = freshLast(metrics.cpu, metrics, age);
      const used = freshLast(metrics.memoryUsed, metrics, age);
      const total = freshLast(metrics.memoryTotal, metrics, age);
      const disk = freshLast(metrics.disk, metrics, age, DISK_SAMPLE_MS);
      const now = {
        type: "meters",
        cpu: cpu?.value ?? null,
        memory: used && total ? usage(used.value, total.value) : null,
        disk: disk ? usage(disk.total - disk.free, disk.total) : null,
      } as const;
      return now.cpu === null && now.memory === null && now.disk === null ? { type: "no_readings" } : now;
    }
  }
}
