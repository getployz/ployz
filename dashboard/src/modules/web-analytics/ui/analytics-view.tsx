import { useQuery } from "@tanstack/react-query";
import { ChartLineIcon, XIcon } from "lucide-react";
import { type ReactNode, Suspense, useDeferredValue } from "react";
import { MetricChart, useMetricCursor } from "#/components/metrics/metric-chart";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card } from "#/components/ui/card";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { Skeleton } from "#/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { cn } from "#/lib/utils";
import { localDay } from "../local-time";
import type { Filter } from "../model";
import {
  formatCount,
  formatDuration,
  formatPercent,
  pageFilterParam,
  parsePageFilter,
  type RangeSpec,
  viewsPerVisit,
} from "./analytics-model";
import { type AnalyticsScope, type AnalyticsView, analyticsViewQueryOptions, useAnalyticsView, useTurnOnAnalytics } from "./analytics.queries";
import { AnalyticsGoals } from "./analytics-goals";
import { RangePicker } from "./range-picker";

export type AnalyticsSearch = { readonly range: RangeSpec; readonly page?: string };

type AnalyticsPageProps = {
  scope: AnalyticsScope;
  search: AnalyticsSearch;
  onSearch: (search: AnalyticsSearch) => void;
};

/**
 * The Analytics place: the plan and the Environment's switch decide whether it reads anything. The controls show at
 * once and each card waits for its own figures. A new range or filter keeps the figures shown, dimmed, until its own
 * arrive.
 */
export function AnalyticsPage({ scope, search, onSearch }: AnalyticsPageProps) {
  const shownRange = useDeferredValue(search.range);
  const shownPage = useDeferredValue(search.page);
  const view = useQuery(analyticsViewQueryOptions(scope, shownRange, shownPage)).data;
  if (view?.gate === "self-host-off") return <SelfHostOff />;
  if (view?.gate === "site-off") return <SiteOff scope={scope} />;
  if (view?.empty) {
    return (
      <div className="flex min-w-0 flex-col gap-4">
        {view.lostDays.length > 0 ? <VisitsLost /> : <NoVisitsYet />}
        <AnalyticsGoals scope={scope} goals={view.goals} visitors={0} />
      </div>
    );
  }
  const read = { scope, range: shownRange, page: shownPage };
  const filter = parsePageFilter(search.page);
  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        <RangePicker range={search.range} timezone={view?.timezone} onRange={(range) => onSearch({ ...search, range })} />
        {filter ? <PageFilterChip filter={filter} onFilter={(page) => onSearch({ range: search.range, ...(page && { page }) })} /> : null}
        {view?.clamped ? <span className="text-xs text-muted-foreground">Starts at the oldest day kept</span> : null}
      </div>
      <div className={cn("flex min-w-0 flex-col gap-4 transition-opacity", (shownRange !== search.range || shownPage !== search.page) && "opacity-50")}>
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-5">
          {OVERVIEW.map((card) => (
            <Section key={card.label} read={read} fallback={<OverviewCard label={card.label} value={<Skeleton className="h-7 w-16" />} />}>
              {(loaded) => <OverviewCard label={card.label} value={card.value(loaded.overview)} />}
            </Section>
          ))}
        </div>
        <Section read={read} fallback={<VisitorsChartSkeleton />}>{(loaded) => <VisitorsChart view={loaded} />}</Section>
        <div className="grid min-w-0 gap-4 lg:grid-cols-2">
          <Section read={read} fallback={<BreakdownSkeleton title="Pages" />}>
            {(loaded) => <Breakdown title="Pages" rows={loaded.pages} onPick={(value) => onSearch({ ...search, page: pageFilterParam({ key: "page", op: "eq", value }) })} />}
          </Section>
          <Section read={read} fallback={<BreakdownSkeleton title="Sources" />}>{(loaded) => <Breakdown title="Sources" rows={loaded.sources} />}</Section>
          <Section read={read} fallback={<BreakdownSkeleton title="Entry pages" />}>{(loaded) => <EntryPages rows={loaded.entryPages} />}</Section>
          <Section read={read} fallback={<BreakdownSkeleton title="Campaigns" />}>{(loaded) => <Breakdown title="Campaigns" rows={loaded.campaigns} />}</Section>
          <Section read={read} fallback={<BreakdownSkeleton title="Languages" />}>{(loaded) => <Breakdown title="Languages" rows={loaded.languages} />}</Section>
          <Section read={read} fallback={<BreakdownSkeleton title="Hosts" />}>{(loaded) => <Breakdown title="Hosts" rows={loaded.hosts} />}</Section>
        </div>
        <Section read={read} fallback={<BreakdownSkeleton title="Goals" />}>
          {(loaded) => <AnalyticsGoals scope={scope} goals={loaded.goals} visitors={loaded.overview.visitors} />}
        </Section>
      </div>
    </div>
  );
}

type OnView = Extract<AnalyticsView, { gate: "on"; empty: false }>;
type ViewRead = { scope: AnalyticsScope; range: RangeSpec; page: string | undefined };

/** One card's own loading: its fallback until the view arrives, then nothing if the page swaps to a gate. */
function Section({ read, fallback, children }: { read: ViewRead; fallback: ReactNode; children: (view: OnView) => ReactNode }) {
  return <Suspense fallback={fallback}><SectionBody read={read}>{children}</SectionBody></Suspense>;
}

function SectionBody({ read, children }: { read: ViewRead; children: (view: OnView) => ReactNode }) {
  const view = useAnalyticsView(read.scope, read.range, read.page);
  return view.gate === "on" && !view.empty ? children(view) : null;
}

const OVERVIEW: ReadonlyArray<{ label: string; value: (overview: OnView["overview"]) => string }> = [
  { label: "Visitors", value: (overview) => formatCount(overview.visitors) },
  { label: "Pageviews", value: (overview) => formatCount(overview.pageviews) },
  { label: "Views per visit", value: (overview) => String(viewsPerVisit(overview.pageviews, overview.visits)) },
  { label: "Bounce rate", value: (overview) => formatPercent(overview.bounceRatePercent) },
  { label: "Visit duration", value: (overview) => formatDuration(overview.avgDurationSeconds) },
];

function OverviewCard({ label, value }: { label: string; value: ReactNode }) {
  return (
    <Card size="sm" className="gap-1 px-3">
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="font-mono text-xl tabular-nums" data-analytics-total={label}>{value}</span>
    </Card>
  );
}

/**
 * Visitors and pageviews per bucket. A partial day draws dashed, joined to its neighbours, so a short day never reads
 * as a drop in traffic.
 */
function VisitorsChart({ view }: { view: OnView }) {
  const cursor = useMetricCursor();
  const partial = new Set(view.partialDays.map((day) => day.day));
  const times = view.buckets.map((bucket) => bucket.at);
  const isPartial = view.buckets.map((bucket) => partial.has(localDay(view.timezone, bucket.at)));
  const touchesPartial = (index: number) => isPartial[index] || isPartial[index - 1] || isPartial[index + 1];
  const split = (values: number[]) => ({
    whole: values.map((value, index) => isPartial[index] ? null : value),
    partial: values.map((value, index) => touchesPartial(index) ? value : null),
  });
  const visitors = split(view.buckets.map((bucket) => bucket.visitors));
  const pageviews = split(view.buckets.map((bucket) => bucket.pageviews));
  const series = [
    { label: "Visitors", color: "var(--chart-1)", values: visitors.whole },
    { label: "Pageviews", color: "var(--chart-2)", values: pageviews.whole },
    ...isPartial.some(Boolean) ? [
      { label: "Visitors", color: "var(--chart-1)", values: visitors.partial, dashed: true },
      { label: "Pageviews", color: "var(--chart-2)", values: pageviews.partial, dashed: true },
    ] : [],
  ];
  const byTime = new Map(view.buckets.map((bucket) => [bucket.at, bucket]));
  return (
    <MetricChart id="analytics-visitors" title="Visitors" subtitle="and pageviews" value={formatCount(view.overview.visitors)}
      times={times} series={series} format={formatCount} deploys={[]} from={view.range.start} to={view.range.end} cursor={cursor}
      height={180}
      renderTooltip={(at) => {
        const bucket = byTime.get(at);
        if (bucket === undefined) return null;
        return (
          <div className="flex flex-col gap-0.5">
            <span>{tooltipTime(view.timezone, view.interval, at)}</span>
            <span>Visitors {formatCount(bucket.visitors)}</span>
            <span>Pageviews {formatCount(bucket.pageviews)}</span>
          </div>
        );
      }} />
  );
}

function VisitorsChartSkeleton() {
  return (
    <Card size="sm" className="gap-2 px-3" aria-busy>
      <div className="flex items-baseline gap-2">
        <h3 className="text-sm font-medium">Visitors</h3>
        <span className="text-xs text-muted-foreground">and pageviews</span>
      </div>
      <Skeleton className="h-[180px]" />
    </Card>
  );
}

function tooltipTime(timezone: string, interval: OnView["interval"], at: number) {
  return new Intl.DateTimeFormat("en-US", interval === "hour"
    ? { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", timeZone: timezone }
    : { weekday: "short", month: "short", day: "numeric", timeZone: timezone }).format(at);
}

type Row = { readonly value: string; readonly visitors: number; readonly pageviews: number };

function Breakdown({ title, rows, onPick }: { title: string; rows: ReadonlyArray<Row>; onPick?: (value: string) => void }) {
  return (
    <BreakdownCard title={title} empty={rows.length === 0}>
      <TableHeader>
        <TableRow>
          <TableHead>{title === "Pages" ? "Page" : title.slice(0, -1)}</TableHead>
          <TableHead className="w-20 text-right">Visitors</TableHead>
          <TableHead className="w-20 text-right">Views</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((row) => (
          <TableRow key={row.value}>
            <ValueCell value={row.value} onPick={onPick} />
            <TableCell className="text-right font-mono tabular-nums">{formatCount(row.visitors)}</TableCell>
            <TableCell className="text-right font-mono tabular-nums">{formatCount(row.pageviews)}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </BreakdownCard>
  );
}

function EntryPages({ rows }: { rows: OnView["entryPages"] }) {
  return (
    <BreakdownCard title="Entry pages" empty={rows.length === 0}>
      <TableHeader>
        <TableRow>
          <TableHead>Page</TableHead>
          <TableHead className="w-20 text-right">Visits</TableHead>
          <TableHead className="w-20 text-right">Bounce</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((row) => (
          <TableRow key={row.value}>
            <ValueCell value={row.value} />
            <TableCell className="text-right font-mono tabular-nums">{formatCount(row.visits)}</TableCell>
            <TableCell className="text-right font-mono tabular-nums">{formatPercent(row.bounceRatePercent)}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </BreakdownCard>
  );
}

function BreakdownCard({ title, empty, children }: { title: string; empty: boolean; children: ReactNode }) {
  return (
    <Card size="sm" className="min-w-0 gap-2 px-3" data-analytics-table={title}>
      <h3 className="text-sm font-medium">{title}</h3>
      {empty ? <p className="py-4 text-center text-xs text-muted-foreground">None in this range</p> : (
        <Table className="table-fixed">{children}</Table>
      )}
    </Card>
  );
}

function BreakdownSkeleton({ title }: { title: string }) {
  return (
    <Card size="sm" className="min-w-0 gap-2 px-3" aria-busy>
      <h3 className="text-sm font-medium">{title}</h3>
      <div className="flex flex-col gap-2 py-1">
        {[0, 1, 2, 3].map((row) => <Skeleton key={row} className="h-5" />)}
      </div>
    </Card>
  );
}

function ValueCell({ value, onPick }: { value: string; onPick?: ((value: string) => void) | undefined }) {
  const text = value === "" ? "Direct" : value;
  return (
    <TableCell className="truncate" title={text}>
      {onPick ? (
        <button type="button" className="max-w-full truncate text-left hover:underline" onClick={() => onPick(value)}>{text}</button>
      ) : text}
    </TableCell>
  );
}

function PageFilterChip({ filter, onFilter }: { filter: Filter; onFilter: (page: string | undefined) => void }) {
  const flipped = { ...filter, op: filter.op === "eq" ? "neq" : "eq" } as const;
  return (
    <Badge variant="secondary" className="h-7 max-w-full gap-1 pr-0.5">
      <button type="button" className="shrink-0 hover:underline" title="Flip the filter"
        onClick={() => onFilter(pageFilterParam(flipped))}>
        {filter.op === "eq" ? "Page is" : "Page is not"}
      </button>
      <span className="truncate font-mono">{filter.value}</span>
      <Button variant="ghost" size="icon-xs" aria-label="Clear the page filter" onClick={() => onFilter(undefined)}><XIcon /></Button>
    </Badge>
  );
}

function Gate({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <Empty className="border" data-analytics-gate={title}>
      <EmptyHeader>
        <EmptyMedia variant="icon"><ChartLineIcon /></EmptyMedia>
        <EmptyTitle>{title}</EmptyTitle>
        {children ? <EmptyDescription>{children}</EmptyDescription> : null}
      </EmptyHeader>
      {action ? <EmptyContent>{action}</EmptyContent> : null}
    </Empty>
  );
}

function SelfHostOff() {
  return (
    <Gate title="Analytics is off">
      Set <code className="font-mono">ANALYTICS_ENABLED=true</code> on this Cloud and restart it to count visits.
    </Gate>
  );
}

function SiteOff({ scope }: { scope: AnalyticsScope }) {
  const turnOn = useTurnOnAnalytics(scope);
  return (
    <Gate title="Analytics is off for this Environment"
      action={<Button disabled={turnOn.isPending} onClick={() => turnOn.mutate()}>Turn on</Button>}>
      Counts visits to this Environment&apos;s domains from the moment it is on, without cookies.
    </Gate>
  );
}

function VisitsLost() {
  return <Gate title="No visits counted" />;
}

function NoVisitsYet() {
  return (
    <Gate title="No visits yet">
      Analytics is on. Numbers appear here after the first visits to this Environment&apos;s domains.
    </Gate>
  );
}
