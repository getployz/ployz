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

// The daemon samples CPU and memory every 10 seconds and disk every 60. A reading counts as now while it is within three
// of its own samples (or three buckets, on a broader read) of the read's end, the freshness `ployz metrics` uses.
const FRESH_SAMPLES = 3;
const HOST_SAMPLE_MS = 10_000;
const DISK_SAMPLE_MS = 60_000;

const usage = (used: number, total: number): Usage | null => total > 0 ? { used, total } : null;

/** What a Server uses as of `metrics`' read, leaving out any reading too old to call current. */
export function serverNow(metrics: ServerMetrics): ServerNow {
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
      const step = metrics.stepMs ?? 0;
      const fresh = <S extends { readonly at: number }>(samples: readonly S[], sampleMs: number) => {
        const last = samples.at(-1);
        return last !== undefined && last.at >= metrics.window.to - FRESH_SAMPLES * Math.max(step, sampleMs) ? last : undefined;
      };
      const cpu = fresh(metrics.cpu, HOST_SAMPLE_MS);
      const used = fresh(metrics.memoryUsed, HOST_SAMPLE_MS);
      const total = fresh(metrics.memoryTotal, HOST_SAMPLE_MS);
      const disk = fresh(metrics.disk, DISK_SAMPLE_MS);
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
