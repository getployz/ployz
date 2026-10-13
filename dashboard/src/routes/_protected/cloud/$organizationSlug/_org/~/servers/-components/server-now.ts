import type { ServerMetrics } from "#/modules/metrics/metrics-model";

type Fraction = number;

export type ServerNow =
  | { readonly type: "meters"; readonly cpu: Fraction | null; readonly memory: Fraction | null; readonly disk: Fraction | null }
  | { readonly type: "needs_upgrade" }
  | { readonly type: "unanswered" }
  | { readonly type: "no_readings" };

const fraction = (used: number | undefined, total: number | undefined) =>
  used === undefined || total === undefined || total <= 0 ? null : used / total;

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
      const disk = metrics.disk.at(-1);
      return {
        type: "meters",
        cpu: metrics.cpu.at(-1)?.value ?? null,
        memory: fraction(metrics.memoryUsed.at(-1)?.value, metrics.memoryTotal.at(-1)?.value),
        disk: disk === undefined ? null : fraction(disk.total - disk.free, disk.total),
      };
    }
  }
}
