import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { MetricMeter } from "#/components/metric-meter";
import { serverMetrics } from "#/modules/metrics/metrics-model";
import { serverMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import type { ServerListItem } from "#/modules/machines/use-servers";
import { NOW_RANGE, serverNow, usageFraction, type ServerNow } from "./server-now";

type Meters = Extract<ServerNow, { type: "meters" }>;

const METERS: readonly { readonly label: string; readonly fraction: (now: Meters) => number | null }[] = [
  { label: "CPU", fraction: (now) => now.cpu },
  { label: "Memory", fraction: (now) => now.memory && usageFraction(now.memory) },
];

/**
 * A Server's CPU and memory now, in a cell the list's container query shows only on a wide row. It reads metrics only
 * while that cell is shown, and stays empty until there is a reading.
 */
export function ServerMeters({ organizationSlug, server }: { organizationSlug: string; server: ServerListItem }) {
  const [shown, setShown] = useState(false);
  const offline = server.status === "offline";
  const read = useQuery({
    ...serverMetricsQueryOptions({ organizationSlug, machineId: server.machine.id, ...NOW_RANGE }),
    enabled: shown && !offline,
  });
  const observe = (cell: HTMLElement) => {
    const observer = new ResizeObserver(([entry]) => setShown((entry?.contentRect.width ?? 0) > 0));
    observer.observe(cell);
    return () => observer.disconnect();
  };
  return (
    <span ref={observe} className="hidden @4xl:flex">
      {offline || !read.isSuccess ? null : (
        <Readings now={serverNow(serverMetrics(read.data, new Map([[server.machine.id, server.name]])), Date.now() - read.dataUpdatedAt)} />
      )}
    </span>
  );
}

function Readings({ now }: { now: ServerNow }) {
  // Disk shows only on the Server's page, so a row with a fresh disk reading alone stays empty too.
  if (now.type !== "meters" || (now.cpu === null && now.memory === null)) return null;
  return (
    <div className="flex gap-4 text-xs text-muted-foreground">
      {METERS.map(({ label, fraction }) => {
        const value = fraction(now);
        return (
          <div key={label} className="flex items-center gap-2">
            <span>{label}</span>
            {value === null ? <span title="Not measured">–</span> : <MetricMeter label={label} fraction={value} className="text-foreground" />}
          </div>
        );
      })}
    </div>
  );
}
