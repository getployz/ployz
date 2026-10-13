import { useQuery } from "@tanstack/react-query";
import { MetricMeter } from "#/components/metric-meter";
import { Skeleton } from "#/components/ui/skeleton";
import { serverMetrics, type ServerMetrics } from "#/modules/metrics/metrics-model";
import { serverMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import type { ServerListItem } from "#/modules/machines/use-servers";

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

const NOW_WINDOW = "5m";

const METERS = [["cpu", "CPU"], ["memory", "Memory"], ["disk", "Disk"]] as const;

export function ServerMeters({ organizationSlug, server }: { organizationSlug: string; server: ServerListItem }) {
  const read = useQuery(serverMetricsQueryOptions({ organizationSlug, machineId: server.machine.id, from: NOW_WINDOW, to: "now" }));
  const cells = "hidden w-[22rem] shrink-0 items-center gap-4 text-xs text-muted-foreground lg:flex";
  if (read.isPending) return <div className={cells}><Skeleton className="h-3 w-full" /></div>;
  const now: ServerNow = read.isError ? { type: "unanswered" } : serverNow(serverMetrics(read.data, new Map([[server.machine.id, server.name]])));
  switch (now.type) {
    case "needs_upgrade":
      return <p className={cells}>Upgrade to see metrics</p>;
    case "unanswered":
      return <p className={cells}>Didn’t answer</p>;
    case "no_readings":
      return <p className={cells}>No readings</p>;
    case "meters":
      return (
        <div className={cells}>
          {METERS.map(([key, label]) => {
            const value = now[key];
            return (
              <div key={key} className="flex w-28 flex-col gap-1">
                <span>{label}</span>
                {value === null ? <span title="Not measured">–</span> : <MetricMeter label={label} fraction={value} className="text-foreground" />}
              </div>
            );
          })}
        </div>
      );
  }
}
