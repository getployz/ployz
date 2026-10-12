import { Suspense, useDeferredValue, useEffect, useState, type ReactNode } from "react";
import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import { CatchBoundary, useLoaderData, type ErrorComponentProps } from "@tanstack/react-router";
import { createChartCursor, type ChartCursorController } from "@tanstack/charts/cursor";
import { XIcon } from "lucide-react";
import type { ServiceListing } from "@ployz/sdk";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { Button } from "#/components/ui/button";
import { Card, CardContent } from "#/components/ui/card";
import { Popover, PopoverContent, PopoverTitle } from "#/components/ui/popover";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "#/components/ui/tooltip";
import { cn } from "#/lib/utils";
import { admission } from "#/modules/config-store/store-deployments";
import { useStoreDeployments } from "#/modules/config-store/store-view.queries";
import { LIMITS, previewMetrics } from "#/modules/uptime/metrics-preview";
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
import { ENVIRONMENT_ROUTE_FROM } from "../../../../-components/environment-route-paths";
import { HealthcheckField, type HealthcheckSetting } from "../HealthcheckField";
import { useTimeZone } from "#/utils/time-zone";
import { atFormat, MetricChart, type MetricSeries, type TimeBand } from "./MetricChart";
import type { MonitoringSection } from "./monitoring-sections";
import { RangePicker } from "./RangePicker";
import { ResponseTooltip } from "./ResponseTooltip";

type MonitoringProps = {
  organizationSlug: string;
  /** Not empty: without a section there is no tab. */
  sections: readonly MonitoringSection[];
  service: ServiceListing;
  range: UptimeRange;
  onRange: (range: UptimeRange) => void;
  addDomain: ReactNode;
  /** Edited in place while the check's path answers with a 4xx. */
  healthcheck: HealthcheckSetting;
};

/**
 * A Service's Monitoring tab: one layout in every state. The status line says what the check sees; the strip and charts
 * stay put and fill in as results arrive. Checks follow each Deploy and run after it; Check now runs one on demand.
 */
export function MonitoringTab(props: MonitoringProps) {
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const scope = { organizationSlug: props.organizationSlug, environmentId };
  const prefetch = usePrefetchServiceUptime(scope, props.service.id);
  const toggle = <RangePicker range={props.range} onRange={props.onRange} onHover={prefetch} />;
  return (
    <CatchBoundary getResetKey={() => props.service.id} errorComponent={MonitoringError}>
      <Suspense fallback={<MonitoringSkeleton toggle={toggle} uptime={props.sections.includes("uptime")} />}>
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

/** The tab's shape before its first read lands: a quiet header, grey bars, and the Response time card's outline. */
function MonitoringSkeleton({ toggle, uptime }: { toggle: ReactNode; uptime: boolean }) {
  return (
    <div className="flex flex-col gap-6" aria-busy>
      <div className="flex flex-wrap items-center justify-between gap-3">
        {uptime ? <StatusLine tone="quiet"><Skeleton className="h-4 w-40" /></StatusLine> : null}
        <div className="ml-auto">{toggle}</div>
      </div>
      {uptime ? (
        <>
          <div className="flex flex-col gap-1.5">
            <div className="flex h-7 items-stretch gap-px">
              {Array.from({ length: 30 }, (_, index) => <div key={index} className="flex-1 rounded-[1px] bg-muted" />)}
            </div>
            <StripEnds from={null} to={null} />
          </div>
          <EmptyChartCard title="Response time" />
        </>
      ) : null}
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
/** An incident the charts zoomed onto, and the range they read for it, fixed when zoomed so each poll reads the same key. */
type Zoom = { readonly incident: UptimeIncidentEntry; readonly range: UptimeRange };

type ViewProps = MonitoringProps & { scope: UptimeScope; toggle: ReactNode; shown: UptimeRange };

/**
 * Status, history, then every metric on one time axis, all over one range. Incidents shade each chart; a click tells the
 * story. A new range keeps the old one, dimmed, until its read lands.
 */
function Monitoring(props: MonitoringProps & { scope: UptimeScope; toggle: ReactNode }) {
  const shown = useDeferredValue(props.range);
  return props.sections.includes("uptime") ? <UptimeMonitoring {...props} shown={shown} /> : <MonitoringView {...props} shown={shown} read={null} />;
}

function UptimeMonitoring(props: ViewProps) {
  const { data, dataUpdatedAt } = useServiceUptime(props.scope, props.service.id, props.shown);
  return <MonitoringView {...props} read={{ uptime: data, at: dataUpdatedAt }} />;
}

/** `read`: the uptime read and when Cloud answered it, or null on a Cloud without the uptime checker. */
function MonitoringView({ organizationSlug, service, sections, range: chosen, shown: range, addDomain, healthcheck, scope, toggle, read }: ViewProps & {
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
  const [cursor] = useState(() => createChartCursor<Date, number>());
  // The library leaves a pinned shared cursor up on outside clicks, so a tap elsewhere or Escape lets it go here.
  useEffect(() => {
    const release = (event: PointerEvent | KeyboardEvent) => {
      if (!cursor.getState()?.pinned) return;
      if (event instanceof KeyboardEvent ? event.key !== "Escape"
        : event.target instanceof Element && event.target.closest("[data-metric-chart], .metric-tooltip")) return;
      cursor.setState(null);
    };
    document.addEventListener("pointerdown", release, true);
    document.addEventListener("keydown", release);
    return () => {
      document.removeEventListener("pointerdown", release, true);
      document.removeEventListener("keydown", release);
    };
  }, [cursor]);
  // When Cloud answered, so the page renders the same on server and client and moves on with each poll.
  const now = read?.at ?? mountedAt;
  const at = atFormat(useTimeZone());
  const state = checkState(uptime);
  const incidents = uptime?.incidents ?? [];
  const checked = checkWindow(resolveRange(range, now));
  // Capped per-result bars begin at the oldest result shown, not at the range.
  const bars = historyBars(checked, uptime);
  const charted = zoom?.range ?? range;
  const window = chartWindow(resolveRange(charted, now), now, (uptime?.check.intervalSeconds ?? 60) * 1000);
  const times = steps(window);
  const showMetrics = sections.includes("metrics");
  const metrics = showMetrics ? previewMetrics(incidents, window, now) : [];
  const deploys = deployments.flatMap((deployment) => {
    const ended = admission(deployment).ended?.getTime();
    const touched = deployment.services.length === 0 || deployment.services.includes(service.name);
    return deployment.status === "applied" && touched && ended !== undefined && ended >= window.from && ended < window.to ? [ended] : [];
  });
  const share = uptimePercent(uptime?.uptime ?? null);
  const open = openIncident(incidents);
  const wrongPath = uptime ? wrongPathIncident(uptime) : null;
  const zoomTo = (incident: UptimeIncidentEntry) => {
    const shown = incidentRange(incident, now);
    setPicked(null);
    setZoom({ incident, range: { from: instantEdge(shown.from), to: shown.to >= now ? "now" : instantEdge(shown.to) } });
  };
  // A chart frame shows the incidents of the read it draws: a zoom reads its own range.
  const frame = (shownIncidents: readonly UptimeIncidentEntry[]): ChartFrame => ({
    times, deploys, from: window.from, to: window.to, cursor,
    bands: shownIncidents.flatMap((incident) => {
      const span = incidentSpan(incident, now);
      return span.to > window.from && span.from < window.to ? [{ id: incident.id, ...span }] : [];
    }),
    incidentAt: (at) => {
      const incident = shownIncidents.find((candidate) => {
        const span = incidentSpan(candidate, now);
        return at >= span.from - window.stepMs && at <= span.to;
      });
      return incident ? (x, y) => setPicked({ incident, x, y }) : null;
    },
  });
  const chart = frame(incidents);
  const column = (key: keyof (typeof metrics)[number]) => metrics.map((row) => row[key]);
  const requests = average(metrics.map((row) => row.ok + row.errors));
  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        {read ? <StatusLine tone={STATUS_TONES[state]}>
          <span className="shrink-0">{STATUS_WORDS[state]}</span>
          <span className="flex min-w-0 items-center gap-1 whitespace-nowrap font-normal text-muted-foreground tabular-nums">
            {state === "no_domain" ? <span className="truncate">· Needs a public domain</span> : null}
            {state === "up" && share !== null ? <span>· {share}</span> : null}
            {open ? (
              <Button variant="link" size="sm" className="min-w-0 shrink text-muted-foreground"
                onClick={(event) => setPicked({ incident: open, x: event.clientX, y: event.clientY })}>
                · {formatSpan(now - Date.parse(open.openedAt))} ·
                {uptime ? <span className="truncate font-mono">{checkedUrl(uptime.check.url)}</span> : null}
                → {causeText(open)}
              </Button>
            ) : null}
            {uptime && !open ? <span className="truncate font-mono">· {checkedUrl(uptime.check.url)}</span> : null}
          </span>
        </StatusLine> : null}
        <div className="ml-auto flex shrink-0 items-center gap-2">
          {read && state === "no_domain" ? addDomain : null}
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
      {read ? <HistoryStrip bars={bars} regions={uptime?.regions ?? {}} at={at} zoomed={zoom?.incident ?? null} onZoom={zoomTo}
        from={at.format(bars[0]?.result ? bars[0].start : checked.from)} to={checked.to >= now ? "Now" : at.format(checked.to)} /> : null}
      <div className={cn("grid gap-3 sm:grid-cols-2", range !== chosen && "opacity-60")}>
        {read ? <div className="sm:col-span-2">
          {uptime === null ? (
            <MetricChart id="response" title="Response time" format={formatMs} {...chart} value="–" series={[]} />
          ) : (
            <CatchBoundary getResetKey={() => `${service.id}:${charted.from}:${charted.to}`} errorComponent={ResponseTimeError}>
              <Suspense fallback={<MetricChart id="response" title="Response time" format={formatMs} {...chart} value="" series={[]} />}>
                <ResponseTime scope={scope} serviceId={service.id} range={charted} window={window}
                  pending={state === "pending" ? `First check within ${formatSpan(uptime.check.intervalSeconds * 1000)}` : null}
                  edge={showMetrics ? column("edgeMs") : null} frame={frame} />
              </Suspense>
            </CatchBoundary>
          )}
        </div> : null}
        {showMetrics ? (
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
            <Evidence incident={picked.incident} regions={uptime?.regions ?? {}} now={now} at={at} onZoom={zoom?.incident.id === picked.incident.id ? null : () => zoomTo(picked.incident)} />
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
  incidentAt: (at: number) => ((x: number, y: number) => void) | null;
};

/**
 * The check's response time, the median across regions, with each region's on hover. A zoom reads its own range, and
 * keeps the old line, dimmed, until that read lands.
 */
function ResponseTime({ scope, serviceId, range, window, pending, edge, frame }: {
  scope: UptimeScope;
  serviceId: string;
  range: UptimeRange;
  window: TimeWindow;
  /** The value while the check has no result yet. */
  pending: string | null;
  edge: readonly (number | null)[] | null;
  frame: (incidents: readonly UptimeIncidentEntry[]) => ChartFrame;
}) {
  const shown = useDeferredValue(range);
  const { data } = useServiceUptime(scope, serviceId, shown);
  const checked = checkSteps(data?.points ?? [], window);
  const check = checked.map((step) => step.medianMs);
  const checkMs = average(check);
  const byAt = new Map(checked.map((step, index) => [step.at, index]));
  const series: MetricSeries[] = [
    ...(edge ? [{ label: "Edge", color: "var(--chart-1)", values: edge }] : []),
    { label: "Check", color: "var(--chart-2)", values: check },
  ];
  return (
    <MetricChart id="response" title="Response time" format={formatMs} {...frame(data?.incidents ?? [])} dimmed={shown !== range}
      value={pending ?? (checkMs === null ? "–" : formatMs(checkMs))} series={series}
      focusAt={checked.flatMap((step) => step.medianMs === null && step.regions.length > 0 ? [step.at] : [])}
      renderTooltip={(at) => {
        const index = byAt.get(at);
        const step = index === undefined ? undefined : checked[index];
        return step ? <ResponseTooltip step={step} end={checked[(index ?? 0) + 1]?.at ?? window.to} edgeMs={edge?.[index ?? -1] ?? null}
          regions={data?.regions ?? {}} /> : null;
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
