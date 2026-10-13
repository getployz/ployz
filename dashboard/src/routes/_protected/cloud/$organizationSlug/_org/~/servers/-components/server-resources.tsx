import { useState, type ReactNode } from "react";
import { Meter } from "@base-ui/react/meter";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { MetricChart, useMetricCursor, type MetricSeries } from "#/components/metrics/metric-chart";
import { Card } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { cn } from "#/lib/utils";
import { bytesText, onGrid, rateText } from "#/modules/metrics/metric-chart-data";
import { serverMetrics, type ServerMetrics } from "#/modules/metrics/metrics-model";
import { serverMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import { chartWindow, steps, type TimeWindow } from "#/modules/uptime/uptime";
import { resolveRange } from "#/modules/uptime/uptime-range";
import { diskSegments, serverUsage, type DiskSegment, type Usage } from "./server-usage";

const RANGES = ["1h", "6h", "1d", "7d", "30d"] as const;
type Range = (typeof RANGES)[number];

const percentText = (fraction: number) => `${Math.round(fraction * 100)}%`;
const usageText = (usage: Usage | null) => usage === null ? "–" : `${bytesText(usage.used)} / ${bytesText(usage.total)}`;

const SEGMENT_COLORS = {
  volumes: "var(--chart-1)",
  mirrors: "var(--chart-2)",
  logsAndMetrics: "var(--chart-3)",
  other: "var(--chart-4)",
  used: "var(--chart-1)",
  free: "var(--muted)",
} satisfies Record<DiskSegment["key"], string>;

/** A Server's CPU, memory, network and disk, read live from the Server for the chosen range. */
export function ServerResources({ organizationSlug, server }: { organizationSlug: string; server: { id: string; name: string } }) {
  const [range, setRange] = useState<Range>("1h");
  const query = useQuery({
    ...serverMetricsQueryOptions({ organizationSlug, machineId: server.id, from: range, to: "now" }),
    placeholderData: keepPreviousData,
  });
  const metrics = query.data ? serverMetrics(query.data, new Map([[server.id, server.name]])) : null;
  const availability = query.isError ? "unanswered" : metrics?.availability.type;

  return (
    <section aria-labelledby="resources-heading" className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <h2 id="resources-heading" className="text-sm font-medium">Resources</h2>
        <ToggleGroup variant="outline" size="sm" spacing={0} value={[range]}
          onValueChange={([next]) => { const picked = RANGES.find((candidate) => candidate === next); if (picked) setRange(picked); }}>
          {RANGES.map((option) => <ToggleGroupItem key={option} value={option}>{option}</ToggleGroupItem>)}
        </ToggleGroup>
      </div>
      {availability === "needs_upgrade" ? <Unavailable>Upgrade {server.name} to see its metrics</Unavailable>
        : availability === "unanswered" || availability === "offline" ? <Unavailable>{server.name} didn’t answer</Unavailable>
        : metrics === null ? <ResourcesSkeleton />
        : <Resources metrics={metrics} answeredAt={query.dataUpdatedAt} range={range} dimmed={query.isPlaceholderData} />}
    </section>
  );
}

function Unavailable({ children }: { children: ReactNode }) {
  return (
    <Card size="sm" className="items-center px-3 py-10 text-center">
      <h3 className="text-sm font-medium">{children}</h3>
    </Card>
  );
}

function ResourcesSkeleton() {
  return (
    <div className="flex flex-col gap-3" aria-busy>
      <div className="grid gap-3 sm:grid-cols-3">
        {[1, 2, 3].map((meter) => <Skeleton key={meter} className="h-24 rounded-xl" />)}
      </div>
      {[1, 2, 3].map((chart) => <Skeleton key={chart} className="h-40 rounded-xl" />)}
    </div>
  );
}

function Resources({ metrics, answeredAt, range, dimmed }: { metrics: ServerMetrics; answeredAt: number; range: Range; dimmed: boolean }) {
  const cursor = useMetricCursor();
  const window = chartWindow(resolveRange({ from: range, to: "now" }, answeredAt), answeredAt, metrics.stepMs ?? 60_000);
  const usage = serverUsage(metrics);
  const frame = { times: steps(window), gaps: metrics.gaps, deploys: [], from: window.from, to: window.to, cursor, dimmed };
  const disk = metrics.disk.at(-1);
  return (
    <div className="flex flex-col gap-3">
      <div className="grid gap-3 sm:grid-cols-3">
        <MeterCard title="CPU" value={usage.cpu === null ? "–" : percentText(usage.cpu)} fraction={usage.cpu} />
        <MeterCard title="Memory" value={usageText(usage.memory)} fraction={share(usage.memory)} aside />
        <MeterCard title="Disk" value={usageText(usage.disk)} fraction={share(usage.disk)} aside />
      </div>
      <MetricChart id="server-cpu" title="CPU" format={percentText} {...frame}
        value={usage.cpu === null ? "–" : percentText(usage.cpu)}
        series={[{ label: "CPU", color: "var(--chart-1)", values: onGrid(metrics.cpu, window) }]} />
      <MetricChart id="server-memory" title="Memory" format={bytesText} {...frame}
        value={usageText(usage.memory)}
        limit={usage.memory && { value: usage.memory.total, label: `Total ${bytesText(usage.memory.total)}` }}
        series={[{ label: "Memory", color: "var(--chart-4)", values: onGrid(metrics.memoryUsed, window) }]} />
      <MetricChart id="server-network" title="Network" format={rateText} {...frame}
        value={networkText(metrics, window)} series={networkSeries(metrics, window)} />
      <DiskCard usage={usage.disk} segments={disk ? diskSegments(disk) : null} dimmed={dimmed} />
    </div>
  );
}

function networkSeries(metrics: ServerMetrics, window: TimeWindow): MetricSeries[] {
  return metrics.interfaces.flatMap((iface, index) => [
    { label: `${iface.name} in`, color: "var(--chart-2)", dashed: index > 0, values: onGrid(iface.netIn, window) },
    { label: `${iface.name} out`, color: "var(--chart-1)", dashed: index > 0, values: onGrid(iface.netOut, window) },
  ]);
}

function networkText(metrics: ServerMetrics, window: TimeWindow) {
  const [first] = metrics.interfaces;
  const out = first ? onGrid(first.netOut, window).filter((value) => value !== null).at(-1) : undefined;
  return first === undefined || out === undefined ? "–" : `${first.name} ${rateText(out)} out`;
}

const share = (usage: Usage | null) => usage && usage.used / usage.total;

/** `aside` shows the share beside the title, for meters whose value is an amount rather than a percent. */
function MeterCard({ title, value, fraction, aside = false }: { title: string; value: string; fraction: number | null; aside?: boolean }) {
  return (
    <Card size="sm" className="gap-2 px-3">
      <div className="flex items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium">{title}</h3>
        {aside && fraction !== null ? <span className="text-xs text-muted-foreground tabular-nums">{percentText(fraction)}</span> : null}
      </div>
      <p className="truncate text-xl font-medium tabular-nums">{value}</p>
      {fraction === null ? <div aria-hidden className="h-1.5 w-full rounded-full bg-muted" /> : (
        <Meter.Root value={Math.min(1, Math.max(0, fraction)) * 100} aria-label={title}>
          <Meter.Track className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
            <Meter.Indicator className={cn("h-full rounded-full", fraction >= 0.85 ? "bg-warning" : "bg-foreground/60")} />
          </Meter.Track>
        </Meter.Root>
      )}
    </Card>
  );
}

function DiskCard({ usage, segments, dimmed }: { usage: Usage | null; segments: readonly DiskSegment[] | null; dimmed: boolean }) {
  return (
    <Card size="sm" className={cn("gap-3 px-3", dimmed && "opacity-60")}>
      <div className="flex items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium">Disk</h3>
        <span className="text-sm text-muted-foreground tabular-nums">{usageText(usage)}</span>
      </div>
      {usage === null || segments === null ? <p className="py-3 text-center text-sm text-muted-foreground">No readings</p> : (
        <>
          <div className="flex h-2 overflow-hidden rounded-full bg-muted" role="img"
            aria-label={segments.map((segment) => `${segment.label} ${bytesText(segment.bytes)}`).join(", ")}>
            {segments.map((segment) => (
              <span key={segment.key} className="h-full" style={{ width: `${(segment.bytes / usage.total) * 100}%`, background: SEGMENT_COLORS[segment.key] }} />
            ))}
          </div>
          <ul className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
            {segments.map((segment) => (
              <li key={segment.key} className="flex items-center gap-1.5">
                <span aria-hidden className="size-2 rounded-full ring-1 ring-foreground/10" style={{ background: SEGMENT_COLORS[segment.key] }} />
                {segment.label}
                <span className="text-foreground tabular-nums">{bytesText(segment.bytes)}</span>
              </li>
            ))}
          </ul>
        </>
      )}
    </Card>
  );
}
