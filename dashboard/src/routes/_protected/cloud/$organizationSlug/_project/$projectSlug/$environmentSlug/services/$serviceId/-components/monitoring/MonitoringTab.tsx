import { Suspense, useDeferredValue, useState, type ReactNode } from "react";
import { useQueryErrorResetBoundary, useSuspenseQuery } from "@tanstack/react-query";
import { CatchBoundary, Link, useLoaderData, type ErrorComponentProps } from "@tanstack/react-router";
import { InfoIcon, XIcon } from "lucide-react";
import type { DeploymentSummary, ServiceListing, VolumeListing } from "@ployz/sdk";
import { MetricChart, atFormat, useMetricCursor, type ChartLimit, type ChartMarker, type MetricCursor, type MetricSeries, type TimeBand, type TimeSpan } from "#/components/metrics/metric-chart";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { Button } from "#/components/ui/button";
import { Card, CardContent } from "#/components/ui/card";
import { Popover, PopoverContent, PopoverTitle } from "#/components/ui/popover";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "#/components/ui/tooltip";
import { cn } from "#/lib/utils";
import { admission } from "#/modules/config-store/store-deployments";
import { useStoreDeployments } from "#/modules/config-store/store-view.queries";
import { volumeStorageText } from "#/modules/config-store/store-volumes";
import { useServerList } from "#/modules/machines/use-servers";
import { bytesText, cpuText, onGrid, rateText } from "#/modules/metrics/metric-chart-data";
import { mergeGaps, serviceMetrics, type Marker, type MetricAvailability, type ServerRef, type ServiceMetrics, type ServiceSeries } from "#/modules/metrics/metrics-model";
import { serviceMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import {
  causeText,
  chartWindow,
  checkState,
  checkSteps,
  formatMs,
  formatSpan,
  historyBars,
  incidentRange,
  incidentSpan,
  observationText,
  openIncident,
  regionLabel,
  steps,
  uptimePercent,
  wrongPathIncident,
  type CheckState,
  type HistoryBar,
  type TimeWindow,
} from "#/modules/uptime/uptime";
import { checkWindow, instantEdge, resolveRange, type UptimeRange } from "#/modules/uptime/uptime-range";
import type { RegionNames, ServiceUptime, UptimeCheckResult, UptimeIncidentEntry } from "#/modules/uptime/uptime-status";
import { type UptimeScope, usePrefetchServiceUptime, useRequestUptimeProbe, useServiceUptime } from "#/modules/uptime/uptime.queries";
import { useTimeZone } from "#/utils/time-zone";
import { ENVIRONMENT_ROUTE_FROM } from "../../../../-components/environment-route-paths";
import { HealthcheckField, type HealthcheckSetting } from "../HealthcheckField";
import { RangePicker } from "./RangePicker";
import { ResponseTooltip } from "./ResponseTooltip";

type MonitoringProps = {
  organizationSlug: string;
  checksUptime: boolean;
  service: ServiceListing;
  range: UptimeRange;
  onRange: (range: UptimeRange) => void;
  /** Edited in place while the check's path answers with a 4xx. */
  healthcheck: HealthcheckSetting;
  vcpuLimit: number | null;
  volume: VolumeListing | null;
};

export function MonitoringTab(props: MonitoringProps) {
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const scope = { organizationSlug: props.organizationSlug, environmentId };
  const prefetch = usePrefetchServiceUptime(scope, props.service.id);
  const toggle = <RangePicker range={props.range} onRange={props.onRange} onHover={prefetch} />;
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
    <RouteErrorAlert title="Monitoring couldn’t load" description="This service’s monitoring is unavailable right now. Try loading it again."
      onRetry={() => { queries.reset(); reset(); }} />
  );
}

function StatusLine({ tone, children }: { tone: "ok" | "bad" | "quiet"; children: ReactNode }) {
  return (
    <h2 className="flex min-w-0 grow basis-40 items-center gap-2 text-base font-medium">
      <span aria-hidden className={cn("inline-block size-2 shrink-0 rounded-full",
        tone === "ok" ? "bg-success" : tone === "bad" ? "bg-destructive" : "bg-muted-foreground")} />
      {children}
    </h2>
  );
}

function StripEnds({ from, to }: { from: ReactNode; to: ReactNode }) {
  return (
    <div className="flex h-4 justify-between text-xs text-muted-foreground">
      <span>{from}</span>
      <span>{to}</span>
    </div>
  );
}

function MonitoringSkeleton({ toggle }: { toggle: ReactNode }) {
  return (
    <div className="flex flex-col gap-6" aria-busy>
      <div className="flex justify-end">{toggle}</div>
      <ChartsSkeleton titles={["CPU", "Memory"]} />
    </div>
  );
}

function ChartsSkeleton({ titles }: { titles: readonly string[] }) {
  return (
    <div className="flex flex-col gap-3" aria-busy>
      {titles.map((title) => <EmptyChartCard key={title} title={title}><Skeleton className="h-30" /></EmptyChartCard>)}
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

function average(values: readonly (number | null)[]) {
  const present = values.flatMap((value) => value === null ? [] : [value]);
  return present.length === 0 ? null : present.reduce((sum, value) => sum + value, 0) / present.length;
}

/** The checked URL without its scheme: "acme.com/health". */
const checkedUrl = (url: string) => url.replace(/^https?:\/\//, "").replace(/\/$/, "");

const STATUS_WORDS = { no_domain: "Not checked", pending: "Checking", up: "Up", unreachable: "Unreachable" } satisfies Record<CheckState, string>;
const STATUS_TONES = { no_domain: "quiet", pending: "quiet", up: "ok", unreachable: "bad" } as const satisfies Record<CheckState, "ok" | "bad" | "quiet">;

type Picked = { readonly incident: UptimeIncidentEntry; readonly x: number; readonly y: number };
/** An incident the charts zoomed onto, and the range they read for it, fixed when zoomed so each poll reads the same key. */
type Zoom = { readonly incident: UptimeIncidentEntry; readonly range: UptimeRange };

type ViewProps = MonitoringProps & { scope: UptimeScope; toggle: ReactNode; shown: UptimeRange };

function Monitoring(props: MonitoringProps & { scope: UptimeScope; toggle: ReactNode }) {
  const shown = useDeferredValue(props.range);
  return props.checksUptime ? <UptimeMonitoring {...props} shown={shown} /> : <MonitoringView {...props} shown={shown} read={null} />;
}

function UptimeMonitoring(props: ViewProps) {
  const { data, dataUpdatedAt } = useServiceUptime(props.scope, props.service.id, props.shown);
  return <MonitoringView {...props} read={{ uptime: data, at: dataUpdatedAt }} />;
}

function MonitoringView({ organizationSlug, service, range: chosen, shown: range, healthcheck, vcpuLimit, volume, scope, toggle, read }: ViewProps & {
  read: { uptime: ServiceUptime | null; at: number } | null;
}) {
  const uptime = read?.uptime ?? null;
  const [mountedAt] = useState(Date.now);
  const checkNow = useRequestUptimeProbe(scope, service.id);
  const checking = checkNow.isPending || (uptime?.probe ? !uptime.probe.answered : false);
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const deployments = useStoreDeployments(organizationSlug, store).data.pages.flatMap((page) => page.deployments);
  const [zoom, setZoom] = useState<Zoom | null>(null);
  const [picked, setPicked] = useState<Picked | null>(null);
  const now = read?.at ?? mountedAt;
  const at = atFormat(useTimeZone());
  const state = checkState(uptime);
  const checked = checkWindow(resolveRange(range, now));
  // Capped per-result bars begin at the oldest result shown, not at the range.
  const bars = historyBars(checked, uptime);
  const share = uptimePercent(uptime?.uptime ?? null);
  const open = openIncident(uptime?.incidents ?? []);
  const wrongPath = uptime ? wrongPathIncident(uptime) : null;
  const zoomTo = (incident: UptimeIncidentEntry) => {
    const shown = incidentRange(incident, now);
    setPicked(null);
    setZoom({ incident, range: { from: instantEdge(shown.from), to: shown.to >= now ? "now" : instantEdge(shown.to) } });
  };
  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        {uptime ? <StatusLine tone={STATUS_TONES[state]}>
          <span className="shrink-0">{STATUS_WORDS[state]}</span>
          <span className="flex min-w-0 items-center gap-1 whitespace-nowrap font-normal text-muted-foreground tabular-nums">
            {state === "up" && share !== null ? <span>· {share}</span> : null}
            {open ? (
              <Button variant="link" size="sm" className="min-w-0 shrink text-muted-foreground"
                onClick={(event) => setPicked({ incident: open, x: event.clientX, y: event.clientY })}>
                · {formatSpan(now - Date.parse(open.openedAt))} ·
                <span className="truncate font-mono">{checkedUrl(uptime.check.url)}</span>
                → {causeText(open)}
              </Button>
            ) : <span className="truncate font-mono">· {checkedUrl(uptime.check.url)}</span>}
          </span>
        </StatusLine> : null}
        <div className="ml-auto flex shrink-0 items-center gap-2">
          {uptime ? (
            <Button variant="outline" size="sm" disabled={checking} onClick={() => checkNow.mutate()}>
              {checking ? <Spinner data-icon="inline-start" /> : null}
              {checking ? "Checking" : "Check now"}
            </Button>
          ) : null}
          {zoom ? (
            <Button variant="secondary" size="sm" onClick={() => setZoom(null)}>
              {at.format(Date.parse(zoom.incident.openedAt))} · {causeText(zoom.incident)}
              <XIcon data-icon="inline-end" />
            </Button>
          ) : toggle}
        </div>
      </div>
      {wrongPath && uptime ? (
        <Card size="sm">
          <CardContent>
            <HealthcheckField {...healthcheck} label="Healthcheck path" timeout={false}
              description={<>Your app answers <span className="font-mono">{new URL(uptime.check.url).pathname}</span> with {causeText(wrongPath)}.</>} />
          </CardContent>
        </Card>
      ) : null}
      {uptime ? <HistoryStrip bars={bars} regions={uptime.regions} at={at} zoomed={zoom?.incident ?? null} onZoom={zoomTo}
        from={at.format(bars[0]?.result ? bars[0].start : checked.from)} to={checked.to >= now ? "Now" : at.format(checked.to)} /> : null}
      <CatchBoundary getResetKey={() => service.id} errorComponent={ChartsError}>
        <Suspense fallback={<ChartsSkeleton titles={uptime ? ["Response time", "CPU"] : ["CPU", "Memory"]} />}>
          <Charts organizationSlug={organizationSlug} scope={scope} service={service} range={zoom?.range ?? range}
            dimmed={range !== chosen} uptime={uptime} uptimeAt={read?.at ?? null} deployments={deployments}
            vcpuLimit={vcpuLimit} volume={volume} onPick={setPicked} />
        </Suspense>
      </CatchBoundary>
      <Popover open={picked !== null} onOpenChange={(next) => { if (!next) setPicked(null); }}>
        {picked ? (
          <PopoverContent side="bottom" align="start"
            anchor={{ getBoundingClientRect: () => DOMRect.fromRect({ x: picked.x, y: picked.y, width: 0, height: 0 }) }}>
            <Evidence incident={picked.incident} regions={uptime?.regions ?? {}} now={now} at={at} onZoom={zoom?.incident.id === picked.incident.id ? null : () => zoomTo(picked.incident)} />
          </PopoverContent>
        ) : null}
      </Popover>
    </div>
  );
}

function ChartsError({ reset }: ErrorComponentProps) {
  const queries = useQueryErrorResetBoundary();
  return (
    <RouteErrorAlert title="Metrics couldn’t load" description="Try loading them again."
      onRetry={() => { queries.reset(); reset(); }} />
  );
}

type ChartFrame = {
  times: readonly number[];
  bands: readonly TimeBand[];
  gaps: readonly TimeSpan[];
  markers: readonly ChartMarker[];
  deploys: readonly number[];
  from: number;
  to: number;
  cursor: MetricCursor;
  incidentAt: (at: number) => ((x: number, y: number) => void) | null;
};

type SeriesKey = keyof ServiceSeries;
type ChartLine = { readonly key: SeriesKey; readonly label: string; readonly color: string };

const RESOURCE_CHARTS = [
  { id: "cpu", title: "CPU", format: (vcpus: number) => `${cpuText(vcpus)} vCPU`, lines: [{ key: "cpu", label: "CPU", color: "var(--chart-1)" }] },
  { id: "memory", title: "Memory", format: bytesText, lines: [{ key: "memory", label: "Memory", color: "var(--chart-4)" }] },
  { id: "network", title: "Network", format: rateText, lines: [{ key: "netIn", label: "In", color: "var(--chart-2)" }, { key: "netOut", label: "Out", color: "var(--chart-1)" }] },
  { id: "disk", title: "Disk", format: rateText, lines: [{ key: "diskRead", label: "Read", color: "var(--chart-3)" }, { key: "diskWrite", label: "Write", color: "var(--chart-5)" }] },
] as const satisfies readonly { id: string; title: string; format: (value: number) => string; lines: readonly ChartLine[] }[];

const MARKER_WORDS = {
  oom: () => "OOM",
  exit: (marker: Marker) => marker.exitCode === undefined ? "Exit" : `Exit ${marker.exitCode}`,
  restart: () => "Restart",
} satisfies Record<Marker["kind"], (marker: Marker) => string>;

const latestOf = (values: readonly (number | null)[]) => values.filter((value) => value !== null).at(-1) ?? null;

const sameRange = (left: UptimeRange, right: UptimeRange) => left.from === right.from && left.to === right.to;

function Charts({ organizationSlug, scope, service, range, dimmed, uptime, uptimeAt, deployments, vcpuLimit, volume, onPick }: {
  organizationSlug: string;
  scope: UptimeScope;
  service: ServiceListing;
  range: UptimeRange;
  dimmed: boolean;
  uptime: ServiceUptime | null;
  uptimeAt: number | null;
  deployments: readonly DeploymentSummary[];
  vcpuLimit: number | null;
  volume: VolumeListing | null;
  onPick: (picked: Picked) => void;
}) {
  const shown = useDeferredValue(range);
  const { data: read, dataUpdatedAt } = useSuspenseQuery(serviceMetricsQueryOptions({
    organizationSlug, environmentId: scope.environmentId, service: service.name, from: shown.from, to: shown.to,
  }));
  const names = new Map(useServerList(organizationSlug).servers.map((row) => [row.machine.id, row.name]));
  const metrics = serviceMetrics(read, names);
  const cursor = useMetricCursor();
  const [split, setSplit] = useState(false);
  const answeredAt = uptimeAt ?? dataUpdatedAt;
  const coarserStepMs = Math.max((uptime?.check.intervalSeconds ?? 60) * 1000, metrics.stepMs ?? 0);
  const window = chartWindow(resolveRange(range, answeredAt), answeredAt, coarserStepMs);
  const times = steps(window);
  const deploys = deployments.flatMap((deployment) => {
    const ended = admission(deployment).ended?.getTime();
    const touched = deployment.services.length === 0 || deployment.services.includes(service.name);
    return deployment.status === "applied" && touched && ended !== undefined && ended >= window.from && ended < window.to ? [ended] : [];
  });
  const markers = metrics.markers.map((marker): ChartMarker => ({ at: marker.at, kind: marker.kind, label: `${MARKER_WORDS[marker.kind](marker)} · ${marker.server.name}` }));
  const frame = (incidents: readonly UptimeIncidentEntry[]): ChartFrame => ({
    times, deploys, markers, gaps: metrics.allGaps, from: window.from, to: window.to, cursor,
    bands: incidents.flatMap((incident) => {
      const span = incidentSpan(incident, answeredAt);
      return span.to > window.from && span.from < window.to ? [{ id: incident.id, ...span }] : [];
    }),
    incidentAt: (at) => {
      const incident = incidents.find((candidate) => {
        const span = incidentSpan(candidate, answeredAt);
        return at >= span.from - window.stepMs && at <= span.to;
      });
      return incident ? (x, y) => onPick({ incident, x, y }) : null;
    },
  });
  const chart = frame(uptime?.incidents ?? []);
  const perServer = split && metrics.byServer.length > 1;
  const memoryLimit = latestOf(metrics.all.memoryLimit.map((sample) => sample.value));
  const limits = {
    cpu: vcpuLimit === null ? null : { value: vcpuLimit, label: `Limit ${cpuText(vcpuLimit)} vCPU` },
    memory: memoryLimit === null || memoryLimit <= 0 ? null : { value: memoryLimit, label: `Limit ${bytesText(memoryLimit)}` },
    network: null,
    disk: null,
  } satisfies Record<(typeof RESOURCE_CHARTS)[number]["id"], ChartLimit | null>;
  const lines = (chartLines: readonly ChartLine[]): MetricSeries[] => perServer
    ? metrics.byServer.flatMap((entry, index) => chartLines.map((line) => ({
      label: chartLines.length > 1 ? `${entry.server.name} ${line.label.toLowerCase()}` : entry.server.name,
      color: line.color, dashed: index > 0, gaps: entry.gaps, values: onGrid(entry.series[line.key], window),
    })))
    : chartLines.map((line) => ({ label: line.label, color: line.color, values: onGrid(metrics.all[line.key], window) }));
  const resourceGaps = perServer ? mergeGaps(metrics.byServer.flatMap((entry) => entry.gaps)) : metrics.allGaps;
  const isDimmed = dimmed || !sameRange(shown, range);
  return (
    <div className={cn("flex flex-col gap-3", isDimmed && "opacity-60")}>
      {uptime ? (
        <CatchBoundary getResetKey={() => `${service.id}:${range.from}:${range.to}`} errorComponent={ResponseTimeError}>
          <Suspense fallback={<MetricChart id="response" title="Response time" subtitle="from uptime checks" format={formatMs} {...chart} value="" series={[]} empty={null} />}>
            <ResponseTime scope={scope} serviceId={service.id} range={range} window={window}
              pending={checkState(uptime) === "pending" ? `First check within ${formatSpan(uptime.check.intervalSeconds * 1000)}` : null}
              frame={frame} />
          </Suspense>
        </CatchBoundary>
      ) : null}
      <AvailabilityLine organizationSlug={organizationSlug} availability={metrics.availability}
        toggle={metrics.byServer.length > 1 ? (
          <ToggleGroup variant="outline" size="sm" spacing={0} value={[perServer ? "server" : "all"]}
            onValueChange={([next]) => { if (next === "all" || next === "server") setSplit(next === "server"); }}>
            <ToggleGroupItem value="all">All</ToggleGroupItem>
            <ToggleGroupItem value="server">By server</ToggleGroupItem>
          </ToggleGroup>
        ) : null} />
      {metrics.availability.type === "needs_upgrade" || metrics.availability.type === "offline"
        ? <MetricsUnavailable organizationSlug={organizationSlug} availability={metrics.availability} />
        : RESOURCE_CHARTS.map((resource) => (
          <MetricChart key={resource.id} id={resource.id} title={resource.title} format={resource.format} {...chart} gaps={resourceGaps}
            value={valueText(resource.lines, metrics, window, resource.format)} series={lines(resource.lines)}
            limit={perServer ? null : limits[resource.id]} />
        ))}
      {volume?.storage.kind === "provisioned" ? (
        <Card size="sm" className="flex-row items-baseline justify-between gap-3 px-3">
          <h3 className="text-sm font-medium">Managed volume</h3>
          <span className="flex min-w-0 gap-2 text-sm text-muted-foreground">
            <span className="truncate font-mono">{volume.name}</span>
            <span className="shrink-0">· {volumeStorageText(volume.storage)}</span>
          </span>
        </Card>
      ) : null}
    </div>
  );
}

function valueText(chartLines: readonly ChartLine[], metrics: ServiceMetrics, window: TimeWindow, format: (value: number) => string) {
  const values = chartLines.map((line) => ({ label: line.label, value: latestOf(onGrid(metrics.all[line.key], window)) }));
  if (values.every((entry) => entry.value === null)) return "–";
  return values.length === 1
    ? format(values[0]?.value ?? 0)
    : values.map((entry) => `${entry.label} ${entry.value === null ? "–" : format(entry.value)}`).join(" · ");
}

const LINK_CLASS = "underline underline-offset-4";

function ServersText({ organizationSlug, servers }: { organizationSlug: string; servers: readonly ServerRef[] }) {
  const [only] = servers;
  if (only === undefined) return null;
  if (servers.length > 1) {
    return <Link to="/cloud/$organizationSlug/~/servers" params={{ organizationSlug }} className={LINK_CLASS}>{servers.length} servers</Link>;
  }
  return <Link to="/cloud/$organizationSlug/~/servers/$serverId" params={{ organizationSlug, serverId: only.id }} className={LINK_CLASS}>{only.name}</Link>;
}

function AvailabilityLine({ organizationSlug, availability, toggle }: { organizationSlug: string; availability: MetricAvailability; toggle: ReactNode }) {
  const lacking = availability.type === "mixed" || availability.type === "unanswered" ? availability.needsUpgrade : [];
  const silent = availability.type === "mixed" ? availability.unanswered : availability.type === "unanswered" ? availability.servers : [];
  if (lacking.length === 0 && silent.length === 0 && toggle === null) return null;
  return (
    <div className="flex min-h-7 flex-wrap items-center justify-between gap-3">
      <p className="flex min-w-0 flex-wrap items-center gap-x-1.5 text-xs text-muted-foreground">
        {lacking.length > 0 || silent.length > 0 ? <InfoIcon aria-hidden className="size-3.5 shrink-0" /> : null}
        {lacking.length > 0 ? (
          <span>
            <ServersText organizationSlug={organizationSlug} servers={lacking} />
            {lacking.length > 1 ? " need an upgrade to show their metrics." : " needs an upgrade to show its metrics."}
          </span>
        ) : null}
        {silent.length > 0 ? <span><ServersText organizationSlug={organizationSlug} servers={silent} /> didn’t answer.</span> : null}
      </p>
      {toggle}
    </div>
  );
}

function MetricsUnavailable({ organizationSlug, availability }: {
  organizationSlug: string;
  availability: Extract<MetricAvailability, { type: "needs_upgrade" | "offline" }>;
}) {
  const servers = availability.type === "needs_upgrade" ? availability.servers : [];
  const [only] = servers;
  const title = availability.type === "offline" ? "Your servers are offline"
    : only !== undefined && servers.length === 1 ? `Upgrade ${only.name} to see its metrics`
    : `Upgrade ${servers.length} servers to see their metrics`;
  const link = availability.type === "offline"
    ? <Link to="/cloud/$organizationSlug/~/servers" params={{ organizationSlug }} className={LINK_CLASS}>Check your servers</Link>
    : only !== undefined && servers.length === 1
      ? <Link to="/cloud/$organizationSlug/~/servers/$serverId" params={{ organizationSlug, serverId: only.id }} className={LINK_CLASS}>Open {only.name}</Link>
      : <Link to="/cloud/$organizationSlug/~/servers" params={{ organizationSlug }} className={LINK_CLASS}>Open Servers</Link>;
  return (
    <Card size="sm" className="items-center gap-1 px-3 py-10 text-center">
      <h3 className="text-sm font-medium">{title}</h3>
      <p className="text-sm text-muted-foreground">{link}</p>
    </Card>
  );
}

/**
 * The check's response time, the median across regions, with each region's on hover. A zoom reads its own range, and
 * keeps the old line, dimmed, until that read lands.
 */
function ResponseTime({ scope, serviceId, range, window, pending, frame }: {
  scope: UptimeScope;
  serviceId: string;
  range: UptimeRange;
  window: TimeWindow;
  /** The value while the check has no result yet. */
  pending: string | null;
  frame: (incidents: readonly UptimeIncidentEntry[]) => ChartFrame;
}) {
  const shown = useDeferredValue(range);
  const { data } = useServiceUptime(scope, serviceId, shown);
  const checked = checkSteps(data?.points ?? [], window);
  const check = checked.map((step) => step.medianMs);
  const checkMs = average(check);
  const byAt = new Map(checked.map((step, index) => [step.at, index]));
  return (
    <MetricChart id="response" title="Response time" subtitle="from uptime checks" format={formatMs} {...frame(data?.incidents ?? [])}
      gaps={[]} empty={pending === null ? "No readings" : null} dimmed={!sameRange(shown, range)} value={pending ?? (checkMs === null ? "–" : formatMs(checkMs))}
      series={[{ label: "Check", color: "var(--chart-2)", values: check }]}
      focusAt={checked.flatMap((step) => step.medianMs === null && step.regions.length > 0 ? [step.at] : [])}
      renderTooltip={(at) => {
        const index = byAt.get(at);
        const step = index === undefined ? undefined : checked[index];
        return step ? <ResponseTooltip step={step} end={checked[(index ?? 0) + 1]?.at ?? window.to} regions={data?.regions ?? {}} /> : null;
      }} />
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

const KIND_WORDS = { routine: null, triggered: "Triggered", confirm: "Confirm", recheck: "Recheck" } as const;

/** A result in a few words: "Amsterdam · 212 ms", "sin · 502 · Confirm". */
function ResultText({ result, regions }: { result: UptimeCheckResult; regions: RegionNames }) {
  const label = regionLabel(regions, result.region);
  const answer = result.verdict === "inconclusive" ? "Inconclusive"
    : result.verdict === "pass" ? formatMs(result.ms)
    : result.status === 0 ? "No response" : String(result.status);
  return (
    <span>
      {label ? <><span className={label.named ? undefined : "font-mono"}>{label.text}</span> · </> : null}
      {[answer, KIND_WORDS[result.kind]].filter(Boolean).join(" · ")}
    </span>
  );
}

const TONES = { up: "bg-success", down: "bg-destructive", none: "bg-muted" } as const satisfies Record<HistoryBar["tone"], string>;

/** The check window in bars, red where a check failed or an incident touched. Hover tells it; click zooms onto its incident. */
function HistoryStrip({ bars, regions, at, from, to, zoomed, onZoom }: {
  bars: readonly HistoryBar[];
  regions: RegionNames;
  at: Intl.DateTimeFormat;
  from: string;
  to: string;
  zoomed: UptimeIncidentEntry | null;
  onZoom: (incident: UptimeIncidentEntry) => void;
}) {
  return (
    <TooltipProvider>
      <div className="flex flex-col gap-1.5">
        <div className="flex h-7 items-stretch gap-px" aria-label="History">
          {bars.map((bar) => {
            const label = at.format(bar.start);
            const first = bar.incidents[0];
            const tone = cn("flex-1 rounded-[1px]", TONES[bar.tone]);
            return (
              <Tooltip key={`${bar.start}-${bar.result?.region}-${bar.result?.kind}`}>
                <TooltipTrigger render={first
                  ? <button type="button" aria-label={label} onClick={() => onZoom(first)}
                    className={cn(tone, "cursor-pointer hover:opacity-80",
                      zoomed && bar.incidents.some((incident) => incident.id === zoomed.id) && "ring-2 ring-foreground")} />
                  : <span className={tone} />
                } />
                <TooltipContent className="flex-col items-start gap-0.5">
                  <span className="font-medium">{label}</span>
                  {bar.result ? <ResultText result={bar.result} regions={regions} /> : null}
                  {bar.incidents.length === 0
                    ? bar.result ? null : <span>{bar.tone === "up" ? "No incidents" : "No data"}</span>
                    : bar.incidents.map((incident) => (
                      <span key={incident.id}>
                        {incident.closedAt === null ? "Ongoing" : formatSpan(Date.parse(incident.closedAt) - Date.parse(incident.openedAt))} · {causeText(incident)}
                      </span>
                    ))}
                </TooltipContent>
              </Tooltip>
            );
          })}
        </div>
        <StripEnds from={from} to={to} />
      </div>
    </TooltipProvider>
  );
}

/** What opened an incident: when, how long, and what each region got back. */
function Evidence({ incident, regions, now, at, onZoom }: {
  incident: UptimeIncidentEntry;
  regions: RegionNames;
  now: number;
  at: Intl.DateTimeFormat;
  onZoom: (() => void) | null;
}) {
  const span = incidentSpan(incident, now);
  return (
    <>
      <PopoverTitle className="flex items-baseline justify-between gap-3">
        <span>{causeText(incident)}</span>
        <span className="font-normal text-muted-foreground">{incident.closedAt === null ? "Ongoing" : formatSpan(span.to - span.from)}</span>
      </PopoverTitle>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Since</dt>
        <dd>{at.format(span.from)}</dd>
        <dt className="text-muted-foreground">{Object.keys(regions).length > 1 ? "Regions" : "Checks"}</dt>
        <dd className="flex flex-col">
          {incident.observations.map((observation) => {
            const label = regionLabel(regions, observation.region);
            return (
              <span key={`${observation.region}-${observation.at}`} className="flex gap-2">
                {label ? <span className={label.named ? undefined : "font-mono"}>{label.text}</span> : null}
                <span className={observation.verdict === "fail" ? "text-destructive" : "text-muted-foreground"}>{observationText(observation)}</span>
              </span>
            );
          })}
        </dd>
      </dl>
      {onZoom ? <Button variant="outline" size="sm" className="self-start" onClick={onZoom}>Zoom in</Button> : null}
    </>
  );
}
