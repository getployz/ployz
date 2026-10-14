import { useQuery } from "@tanstack/react-query";
import { MetricMeter } from "#/components/metric-meter";
import { useMediaQuery } from "#/hooks/use-media-query";
import { serverMetrics } from "#/modules/metrics/metrics-model";
import { serverMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import type { ServerListItem } from "#/modules/machines/use-servers";
import { NOW_RANGE, serverNow, usageFraction, type ServerNow } from "./server-now";

// The list's container query decides when meters show; this only keeps phones from reading what they never show.
const WIDE = "(min-width: 64rem)";

type Meters = Extract<ServerNow, { type: "meters" }>;

const METERS: readonly { readonly label: string; readonly fraction: (now: Meters) => number | null }[] = [
  { label: "CPU", fraction: (now) => now.cpu },
  { label: "Memory", fraction: (now) => now.memory && usageFraction(now.memory) },
];

/** A Server's CPU and memory now, or nothing until there is a reading to show. */
export function ServerMeters({ organizationSlug, server }: { organizationSlug: string; server: ServerListItem }) {
  const wide = useMediaQuery(WIDE);
  const offline = server.status === "offline";
  const read = useQuery({
    ...serverMetricsQueryOptions({ organizationSlug, machineId: server.machine.id, ...NOW_RANGE }),
    enabled: wide && !offline,
  });
  if (offline || !read.isSuccess) return null;
  const now = serverNow(serverMetrics(read.data, new Map([[server.machine.id, server.name]])), Date.now() - read.dataUpdatedAt);
  if (now.type !== "meters") return null;
  return (
    <div className="flex gap-4 text-xs text-muted-foreground">
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
