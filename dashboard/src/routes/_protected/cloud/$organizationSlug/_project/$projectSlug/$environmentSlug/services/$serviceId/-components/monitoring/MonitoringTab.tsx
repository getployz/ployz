import { Suspense, useDeferredValue, useRef, useState, type ReactNode } from "react";
import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import { CatchBoundary, useLoaderData, type ErrorComponentProps } from "@tanstack/react-router";
import { createChartCursor, type ChartCursorController } from "@tanstack/charts/cursor";
import { XIcon } from "lucide-react";
import type { ServiceListing } from "@ployz/sdk";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { Button } from "#/components/ui/button";
import { Card } from "#/components/ui/card";
import { Popover, PopoverContent, PopoverTitle } from "#/components/ui/popover";
import { Skeleton } from "#/components/ui/skeleton";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "#/components/ui/tooltip";
import { cn } from "#/lib/utils";
import { admission } from "#/modules/config-store/store-deployments";
import { useStoreDeployments } from "#/modules/config-store/store-view.queries";
import { LIMITS, previewMetrics } from "#/modules/uptime/metrics-preview";
import {
  causeText,
  checkSeries,
  checkState,
  formatMs,
  formatSpan,
  historyDays,
  incidentSpan,
  incidentWindow,
  observationText,
  openIncident,
  rangeCovering,
  rangeWindow,
  steps,
  uncheckedDays,
  uptimePercent,
  type CheckState,
  type HistoryDay,
  type TimeWindow,
} from "#/modules/uptime/uptime";
import { UPTIME_RANGES, type UptimeIncidentEntry, type UptimeRange } from "#/modules/uptime/uptime-status";
import {
  type UptimeScope,
  usePrefetchServiceUptime,
  useServiceUptime,
  useServiceUptimeSeries,
} from "#/modules/uptime/uptime.queries";
import { ENVIRONMENT_ROUTE_FROM } from "../../../../-components/environment-route-paths";
import { MetricChart, type MetricSeries, type TimeBand } from "./MetricChart";

type MonitoringProps = {
  organizationSlug: string;
  service: ServiceListing;
  range: UptimeRange;
  onRange: (range: UptimeRange) => void;
  addDomain: ReactNode;
};

/**
 * A Service's Monitoring tab: one layout in every state. The status line says what the check sees; the strip and charts
 * stay put and fill in as results arrive. Checks follow each Deploy, so it only reads.
 */
export function MonitoringTab(props: MonitoringProps) {
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const scope = { organizationSlug: props.organizationSlug, environmentId };
  const prefetch = usePrefetchServiceUptime(scope, props.service.id);
  const toggle = <RangeToggle range={props.range} onRange={props.onRange} onHover={prefetch} />;
  return (
    <CatchBoundary getResetKey={() => props.service.id} errorComponent={MonitoringError}>
      <Suspense fallback={<MonitoringSkeleton toggle={toggle} />}>
        <Monitoring {...props} scope={scope} toggle={toggle} />
      </Suspense>
    </CatchBoundary>
  );
}

function MonitoringError({ reset }: ErrorComponentProps) {
  const queries = useQueryErrorResetBoundary();
  return (
    <RouteErrorAlert title="Monitoring couldn’t load" description="This service’s uptime is unavailable right now. Try loading it again."
      onRetry={() => { queries.reset(); reset(); }} />
  );
}

function StatusLine({ tone, children }: { tone: "ok" | "bad" | "quiet"; children: ReactNode }) {
  return (
    <h2 className="flex min-w-0 items-center gap-2 text-base font-medium">
      <span aria-hidden className={cn("inline-block size-2 shrink-0 rounded-full",
        tone === "ok" ? "bg-success" : tone === "bad" ? "bg-destructive" : "bg-muted-foreground")} />
      {children}
    </h2>
  );
}

function RangeToggle({ range, onRange, onHover }: { range: UptimeRange; onRange: (range: UptimeRange) => void; onHover: (range: UptimeRange) => void }) {
  return (
    <ToggleGroup variant="outline" size="sm" spacing={0} value={[range]} aria-label="Range"
      onValueChange={(next) => {
        const chosen = UPTIME_RANGES.find((candidate) => candidate === next[0]);
        if (chosen) onRange(chosen);
      }}>
      {UPTIME_RANGES.map((candidate) => (
        <ToggleGroupItem key={candidate} value={candidate} onPointerEnter={() => onHover(candidate)}>{candidate}</ToggleGroupItem>
      ))}
    </ToggleGroup>
  );
}

const STRIP_ENDS = (
  <div className="flex justify-between text-xs text-muted-foreground">
    <span>90 days ago</span>
    <span>Today</span>
  </div>
);

/** The tab's shape before its first read lands: a quiet header, ninety grey days, and the Response time card's outline. */
function MonitoringSkeleton({ toggle }: { toggle: ReactNode }) {
  return (
    <div className="flex flex-col gap-6" aria-busy>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <StatusLine tone="quiet"><Skeleton className="h-4 w-40" /></StatusLine>
        {toggle}
      </div>
      <div className="flex flex-col gap-1.5">
        <div className="flex h-7 items-stretch gap-px">
          {Array.from({ length: 90 }, (_, index) => <div key={index} className="flex-1 rounded-[1px] bg-muted" />)}
        </div>
        {STRIP_ENDS}
      </div>
      <EmptyChartCard title="Response time" />
    </div>
  );
}

function EmptyChartCard({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <Card size="sm" className="gap-2 px-3">
      <h3 className="text-sm font-medium">{title}</h3>
      {children ?? <div className="h-30" />}
    </Card>
  );
}

const DAY_FORMAT = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", timeZone: "UTC" });
const AT_FORMAT = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });

const unit = (suffix: string, digits = 0) => (value: number) => `${value.toFixed(digits)}${suffix}`;
const latest = (values: readonly (number | null)[]) => values.filter((value) => value !== null).at(-1) ?? null;
function average(values: readonly (number | null)[]) {
  const present = values.flatMap((value) => value === null ? [] : [value]);
  return present.length === 0 ? null : present.reduce((sum, value) => sum + value, 0) / present.length;
}

/** The checked URL without its scheme: "acme.com/health". */
const checkedUrl = (url: string) => url.replace(/^https?:\/\//, "").replace(/\/$/, "");

const STATUS_WORDS = { no_domain: "Not checked", pending: "Checking", up: "Up", unreachable: "Unreachable" } satisfies Record<CheckState, string>;
const STATUS_TONES = { no_domain: "quiet", pending: "quiet", up: "ok", unreachable: "bad" } as const satisfies Record<CheckState, "ok" | "bad" | "quiet">;

type Picked = { readonly incident: UptimeIncidentEntry; readonly x: number; readonly y: number };

/** Ployz collects no Service metrics yet; dev builds draw invented ones so the layout can be judged. */
const PREVIEW_METRICS = import.meta.env.DEV;

/** Status, ninety days of history, then every metric on one time axis. Incidents shade each chart; a click tells the story. */
function Monitoring({ organizationSlug, service, range, addDomain, scope, toggle }: MonitoringProps & { scope: UptimeScope; toggle: ReactNode }) {
  const { data: uptime, dataUpdatedAt } = useServiceUptime(scope, service.id);
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const deployments = useStoreDeployments(organizationSlug, store).data.pages.flatMap((page) => page.deployments);
  const [zoom, setZoom] = useState<UptimeIncidentEntry | null>(null);
  const [picked, setPicked] = useState<Picked | null>(null);
  const [cursor] = useState(() => createChartCursor<Date, number>());
  const pointer = useRef({ x: 0, y: 0 });
  // When Cloud answered, so the page renders the same on server and client and moves on with each poll.
  const now = dataUpdatedAt;
  const state = checkState(uptime);
  const incidents = uptime?.incidents ?? [];
  const window = zoom ? incidentWindow(zoom, now) : rangeWindow(now, range);
  const times = steps(window);
  const metrics = PREVIEW_METRICS ? previewMetrics(incidents, window, now) : [];
  const bands = incidents.flatMap((incident) => {
    const span = incidentSpan(incident, now);
    return span.to > window.from && span.from < window.to ? [{ id: incident.id, ...span }] : [];
  });
  const deploys = deployments.flatMap((deployment) => {
    const ended = admission(deployment).ended?.getTime();
    const touched = deployment.services.length === 0 || deployment.services.includes(service.name);
    return deployment.status === "applied" && touched && ended !== undefined && ended >= window.from && ended < window.to ? [ended] : [];
  });
  const share = uptimePercent(uptime?.uptime[range] ?? null);
  const open = openIncident(incidents);
  const pick = (at: number) => {
    const incident = incidents.find((candidate) => {
      const span = incidentSpan(candidate, now);
      return at >= span.from - window.stepMs && at <= span.to;
    });
    setPicked(incident ? { incident, ...pointer.current } : null);
  };
  const zoomTo = (incident: UptimeIncidentEntry) => {
    setPicked(null);
    setZoom(incident);
  };
  const chart = { times, bands, deploys, from: window.from, to: window.to, cursor, onPick: pick };
  const column = (key: keyof (typeof metrics)[number]) => metrics.map((row) => row[key]);
  const requests = average(metrics.map((row) => row.ok + row.errors));
  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <StatusLine tone={STATUS_TONES[state]}>
          {STATUS_WORDS[state]}
          <span className="flex min-w-0 items-center gap-1 font-normal text-muted-foreground tabular-nums">
            {state === "no_domain" ? <span>· Needs a public domain</span> : null}
            {state === "up" && share !== null ? <span>· {share}</span> : null}
            {open ? (
              <Button variant="link" size="sm" className="text-muted-foreground"
                onClick={(event) => setPicked({ incident: open, x: event.clientX, y: event.clientY })}>
                · {formatSpan(now - Date.parse(open.openedAt))} · {causeText(open)}
              </Button>
            ) : null}
            {uptime && !open ? <span className="truncate font-mono">· {checkedUrl(uptime.check.url)}</span> : null}
          </span>
        </StatusLine>
        <div className="flex items-center gap-2">
          {state === "no_domain" ? addDomain : null}
          {zoom ? (
            <Button variant="secondary" size="sm" onClick={() => setZoom(null)}>
              {AT_FORMAT.format(Date.parse(zoom.openedAt))} · {causeText(zoom)}
              <XIcon data-icon="inline-end" />
            </Button>
          ) : toggle}
        </div>
      </div>
      <HistoryStrip days={historyDays({ days: uptime?.days ?? uncheckedDays(now), incidents }, now)} zoomed={zoom} onZoom={zoomTo} />
      <div className="grid gap-3 sm:grid-cols-2"
        onPointerDownCapture={(event) => { pointer.current = { x: event.clientX, y: event.clientY }; }}>
        <div className="sm:col-span-2">
          {uptime === null ? (
            <MetricChart id="response" title="Response time" format={formatMs} {...chart} value="–" series={[]} />
          ) : (
            <CatchBoundary getResetKey={() => `${service.id}:${zoom ? rangeCovering(window.from, now) : range}`} errorComponent={ResponseTimeError}>
              <Suspense fallback={<MetricChart id="response" title="Response time" format={formatMs} {...chart} value="" series={[]} />}>
                <ResponseTime scope={scope} serviceId={service.id} range={zoom ? rangeCovering(window.from, now) : range} window={window}
                  pending={state === "pending" ? `First check within ${formatSpan(uptime.check.intervalSeconds * 1000)}` : null}
                  edge={PREVIEW_METRICS ? column("edgeMs") : null} chart={chart} />
              </Suspense>
            </CatchBoundary>
          )}
        </div>
        {PREVIEW_METRICS ? (
          <>
            <div className="sm:col-span-2">
              <MetricChart id="requests" title="Requests" value={requests === null ? "–" : `${Math.round(requests)}/min`} format={unit("/min")} {...chart}
                series={[{ label: "2xx", color: "var(--chart-3)", values: column("ok") }, { label: "5xx", color: "var(--chart-5)", values: column("errors") }]} />
            </div>
            <MetricChart id="cpu" title="CPU" value={unit(" vCPU", 2)(latest(column("cpu")) ?? 0)} format={unit("", 1)} limit={LIMITS.cpu} {...chart}
              series={[{ label: "CPU", color: "var(--chart-1)", values: column("cpu") }]} />
            <MetricChart id="memory" title="Memory" value={unit(" GB", 2)(latest(column("memoryGb")) ?? 0)} format={unit(" GB", 1)} limit={LIMITS.memoryGb} {...chart}
              series={[{ label: "Memory", color: "var(--chart-4)", values: column("memoryGb") }]} />
            <MetricChart id="network" title="Network" value={`${Math.round(latest(column("outKbps")) ?? 0)} KB/s`} format={unit(" KB/s")} {...chart}
              series={[{ label: "In", color: "var(--chart-2)", values: column("inKbps") }, { label: "Out", color: "var(--chart-1)", values: column("outKbps") }]} />
            <MetricChart id="disk" title="Disk" value={unit(" GB", 1)(latest(column("diskGb")) ?? 0)} format={unit(" GB")} limit={LIMITS.diskGb} {...chart}
              series={[{ label: "Disk", color: "var(--chart-3)", values: column("diskGb") }]} />
          </>
        ) : null}
      </div>
      <Popover open={picked !== null} onOpenChange={(next) => { if (!next) setPicked(null); }}>
        {picked ? (
          <PopoverContent side="bottom" align="start"
            anchor={{ getBoundingClientRect: () => DOMRect.fromRect({ x: picked.x, y: picked.y, width: 0, height: 0 }) }}>
            <Evidence incident={picked.incident} now={now} onZoom={zoom?.id === picked.incident.id ? null : () => zoomTo(picked.incident)} />
          </PopoverContent>
        ) : null}
      </Popover>
    </div>
  );
}

type ChartFrame = {
  times: readonly number[];
  bands: readonly TimeBand[];
  deploys: readonly number[];
  from: number;
  to: number;
  cursor: ChartCursorController<Date, number>;
  onPick: (at: number) => void;
};

/**
 * The check's response time, averaged across regions. A new range keeps the old line, dimmed, until its read lands,
 * instead of falling back to an empty card.
 */
function ResponseTime({ scope, serviceId, range, window, pending, edge, chart }: {
  scope: UptimeScope;
  serviceId: string;
  range: UptimeRange;
  window: TimeWindow;
  /** The value while the check has no result yet. */
  pending: string | null;
  edge: readonly (number | null)[] | null;
  chart: ChartFrame;
}) {
  const shown = useDeferredValue(range);
  const check = checkSeries(useServiceUptimeSeries(scope, serviceId, shown), window);
  const checkMs = average(check);
  const series: MetricSeries[] = [
    ...(edge ? [{ label: "Edge", color: "var(--chart-1)", values: edge }] : []),
    { label: "Check", color: "var(--chart-2)", values: check },
  ];
  return (
    <MetricChart id="response" title="Response time" format={formatMs} {...chart} dimmed={shown !== range}
      value={pending ?? (checkMs === null ? "–" : formatMs(checkMs))} series={series} />
  );
}

function ResponseTimeError({ reset }: ErrorComponentProps) {
  const queries = useQueryErrorResetBoundary();
  return (
    <EmptyChartCard title="Response time">
      <RouteErrorAlert title="Response time couldn’t load" description="Try loading it again."
        onRetry={() => { queries.reset(); reset(); }} />
    </EmptyChartCard>
  );
}

/** Ninety days, a bar each, red where an incident touched it. Hover tells it; click zooms the charts onto it. */
function HistoryStrip({ days, zoomed, onZoom }: {
  days: readonly HistoryDay[];
  zoomed: UptimeIncidentEntry | null;
  onZoom: (incident: UptimeIncidentEntry) => void;
}) {
  return (
    <TooltipProvider>
      <div className="flex flex-col gap-1.5">
        <div className="flex h-7 items-stretch gap-px" aria-label="History">
          {days.map((day) => {
            const first = day.incidents[0];
            return (
              <Tooltip key={day.start}>
                <TooltipTrigger render={
                  <button type="button" aria-label={DAY_FORMAT.format(day.start)} onClick={() => { if (first) onZoom(first); }}
                    className={cn("flex-1 rounded-[1px]",
                      first ? "cursor-pointer bg-destructive hover:opacity-80" : day.checked ? "cursor-default bg-success" : "cursor-default bg-muted",
                      zoomed && day.incidents.some((incident) => incident.id === zoomed.id) && "ring-2 ring-foreground")} />
                } />
                <TooltipContent className="flex-col items-start gap-0.5">
                  <span className="font-medium">{DAY_FORMAT.format(day.start)}</span>
                  {day.incidents.length === 0
                    ? <span>{day.checked ? "No incidents" : "No data"}</span>
                    : day.incidents.map((incident) => (
                      <span key={incident.id}>
                        {incident.closedAt === null ? "Ongoing" : formatSpan(Date.parse(incident.closedAt) - Date.parse(incident.openedAt))} · {causeText(incident)}
                      </span>
                    ))}
                </TooltipContent>
              </Tooltip>
            );
          })}
        </div>
        {STRIP_ENDS}
      </div>
    </TooltipProvider>
  );
}

/** What opened an incident: when, how long, and what each region got back. */
function Evidence({ incident, now, onZoom }: { incident: UptimeIncidentEntry; now: number; onZoom: (() => void) | null }) {
  const span = incidentSpan(incident, now);
  return (
    <>
      <PopoverTitle className="flex items-baseline justify-between gap-3">
        <span>{causeText(incident)}</span>
        <span className="font-normal text-muted-foreground">{incident.closedAt === null ? "Ongoing" : formatSpan(span.to - span.from)}</span>
      </PopoverTitle>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Since</dt>
        <dd>{AT_FORMAT.format(span.from)}</dd>
        <dt className="text-muted-foreground">Regions</dt>
        <dd className="flex flex-col font-mono">
          {incident.observations.map((observation) => (
            <span key={`${observation.region}-${observation.at}`} className="flex gap-2">
              <span>{observation.region}</span>
              <span className={observation.verdict === "fail" ? "text-destructive" : "text-muted-foreground"}>{observationText(observation)}</span>
            </span>
          ))}
        </dd>
      </dl>
      {onZoom ? <Button variant="outline" size="sm" onClick={onZoom}>Zoom in</Button> : null}
    </>
  );
}
