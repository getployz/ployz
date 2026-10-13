import { useQuery } from "@tanstack/react-query";
import { MetricMeter } from "#/components/metric-meter";
import { Skeleton } from "#/components/ui/skeleton";
import { useMediaQuery } from "#/hooks/use-media-query";
import { serverMetrics } from "#/modules/metrics/metrics-model";
import { serverMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import type { ServerListItem } from "#/modules/machines/use-servers";
import { NOW_RANGE, serverNow, usageFraction, type ServerNow } from "./server-now";

const WIDE = "(min-width: 64rem)";

type Meters = Extract<ServerNow, { type: "meters" }>;

const METERS: readonly { readonly label: string; readonly fraction: (now: Meters) => number | null }[] = [
  { label: "CPU", fraction: (now) => now.cpu },
  { label: "Memory", fraction: (now) => now.memory && usageFraction(now.memory) },
  { label: "Disk", fraction: (now) => now.disk && usageFraction(now.disk) },
];

export function ServerMeters({ organizationSlug, server }: { organizationSlug: string; server: ServerListItem }) {
  const wide = useMediaQuery(WIDE);
  const offline = server.status === "offline";
  const read = useQuery({
    ...serverMetricsQueryOptions({ organizationSlug, machineId: server.machine.id, ...NOW_RANGE }),
    enabled: wide && !offline,
  });
  const cells = "hidden w-[22rem] shrink-0 items-center gap-4 text-xs text-muted-foreground lg:flex";
  if (offline) return <p className={cells}>Didn’t answer</p>;
  if (!wide) return null;
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
          {METERS.map(({ label, fraction }) => {
            const value = fraction(now);
            return (
              <div key={label} className="flex w-28 flex-col gap-1">
                <span>{label}</span>
                {value === null ? <span title="Not measured">–</span> : <MetricMeter label={label} fraction={value} className="text-foreground" />}
              </div>
            );
          })}
        </div>
      );
  }
}
