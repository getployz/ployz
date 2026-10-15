import { Link } from "@tanstack/react-router";
import { ChartLineIcon, XIcon } from "lucide-react";
import { MetricChart, useMetricCursor } from "#/components/metrics/metric-chart";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card } from "#/components/ui/card";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { localDay } from "../local-time";
import type { Filter } from "../model";
import {
  formatCount,
  formatDuration,
  formatPercent,
  pageFilterParam,
  parsePageFilter,
  type PartialDay,
  type RangeSpec,
  viewsPerVisit,
} from "./analytics-model";
import { type AnalyticsScope, type AnalyticsView, useAnalyticsView, useTurnOnAnalytics } from "./analytics.queries";
import { AnalyticsGoals } from "./analytics-goals";
import { RangePicker } from "./range-picker";

export type AnalyticsSearch = { readonly range: RangeSpec; readonly page?: string };

type AnalyticsPageProps = {
  scope: AnalyticsScope;
  search: AnalyticsSearch;
  onSearch: (search: AnalyticsSearch) => void;
};

/** The Analytics place: the plan and the Environment's switch decide whether it reads anything. */
export function AnalyticsPage({ scope, search, onSearch }: AnalyticsPageProps) {
  const view = useAnalyticsView(scope, search.range, search.page);
  if (view.gate === "unpaid") return <Unpaid organizationSlug={scope.organizationSlug} />;
  if (view.gate === "self-host-off") return <SelfHostOff />;
  if (view.gate === "site-off") return <SiteOff scope={scope} />;
  if (view.gate !== "on") return <NoVisitsYet />;
  if (view.empty) {
    return (
      <div className="flex min-w-0 flex-col gap-4">
        {view.lostDays.length > 0 ? <VisitsLost days={view.lostDays} /> : <NoVisitsYet />}
        <AnalyticsGoals scope={scope} goals={view.goals} visitors={0} />
      </div>
    );
  }
  const filter = parsePageFilter(search.page);
  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        <RangePicker range={search.range} timezone={view.timezone} onRange={(range) => onSearch({ ...search, range })} />
        {filter ? <PageFilterChip filter={filter} onFilter={(page) => onSearch({ range: search.range, ...(page && { page }) })} /> : null}
        {view.clamped ? <span className="text-xs text-muted-foreground">Starts at the oldest day kept</span> : null}
      </div>
      <OverviewCards overview={view.overview} />
      <VisitorsChart view={view} />
      <div className="grid min-w-0 gap-4 lg:grid-cols-2">
        <Breakdown title="Pages" rows={view.pages} onPick={(value) => onSearch({ ...search, page: pageFilterParam({ key: "page", op: "eq", value }) })} />
        <Breakdown title="Sources" rows={view.sources} />
        <EntryPages rows={view.entryPages} />
        <Breakdown title="Campaigns" rows={view.campaigns} />
        <Breakdown title="Languages" rows={view.languages} />
        <Breakdown title="Hosts" rows={view.hosts} />
      </div>
      <AnalyticsGoals scope={scope} goals={view.goals} visitors={view.overview.visitors} />
    </div>
  );
}

type OnView = Extract<AnalyticsView, { gate: "on"; empty: false }>;

function OverviewCards({ overview }: { overview: OnView["overview"] }) {
  const cards = [
    { label: "Visitors", value: formatCount(overview.visitors) },
    { label: "Pageviews", value: formatCount(overview.pageviews) },
    { label: "Views per visit", value: String(viewsPerVisit(overview.pageviews, overview.visits)) },
    { label: "Bounce rate", value: formatPercent(overview.bounceRatePercent) },
    { label: "Visit duration", value: formatDuration(overview.avgDurationSeconds) },
  ];
  return (
    <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-5">
      {cards.map((card) => (
        <Card key={card.label} size="sm" className="gap-1 px-3">
          <span className="text-xs text-muted-foreground">{card.label}</span>
          <span className="font-mono text-xl tabular-nums" data-analytics-total={card.label}>{card.value}</span>
        </Card>
      ))}
    </div>
  );
}

const PARTIAL_WORDS = { gap: "Some visits not counted", today: "Still arriving", late: "May still be arriving" } as const;

/**
 * Visitors and pageviews per bucket. A partial day draws dashed, joined to its neighbours, so a short day never reads
 * as a drop in traffic.
 */
function VisitorsChart({ view }: { view: OnView }) {
  const cursor = useMetricCursor();
  const partial = new Map(view.partialDays.map((day) => [day.day, day.reason]));
  const times = view.buckets.map((bucket) => bucket.at);
  const isPartial = view.buckets.map((bucket) => partial.has(localDay(view.timezone, bucket.at)));
  const touchesPartial = (index: number) => isPartial[index] || isPartial[index - 1] || isPartial[index + 1];
  const split = (values: number[]) => ({
    whole: values.map((value, index) => isPartial[index] ? null : value),
    partial: values.map((value, index) => touchesPartial(index) ? value : null),
  });
  const visitors = split(view.buckets.map((bucket) => bucket.visitors));
  const pageviews = split(view.buckets.map((bucket) => bucket.pageviews));
  const anyPartial = isPartial.some(Boolean);
  const series = [
    { label: "Visitors", color: "var(--chart-1)", values: visitors.whole },
    { label: "Pageviews", color: "var(--chart-2)", values: pageviews.whole },
    ...anyPartial ? [
      { label: "Visitors, partial", color: "var(--chart-1)", values: visitors.partial, dashed: true },
      { label: "Pageviews, partial", color: "var(--chart-2)", values: pageviews.partial, dashed: true },
    ] : [],
  ];
  const byTime = new Map(view.buckets.map((bucket) => [bucket.at, bucket]));
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <MetricChart id="analytics-visitors" title="Visitors" subtitle="and pageviews" value={formatCount(view.overview.visitors)}
        times={times} series={series} format={formatCount} deploys={[]} from={view.range.start} to={view.range.end} cursor={cursor}
        height={180}
        renderTooltip={(at) => {
          const bucket = byTime.get(at);
          if (bucket === undefined) return null;
          const reason = partial.get(localDay(view.timezone, at));
          return (
            <div className="flex flex-col gap-0.5">
              <span>{tooltipTime(view.timezone, view.interval, at)}</span>
              <span>Visitors {formatCount(bucket.visitors)}</span>
              <span>Pageviews {formatCount(bucket.pageviews)}</span>
              {reason ? <span className="text-muted-foreground">Partial: {PARTIAL_WORDS[reason]}</span> : null}
            </div>
          );
        }} />
      {anyPartial ? <PartialNote days={view.partialDays} /> : null}
    </div>
  );
}

function tooltipTime(timezone: string, interval: OnView["interval"], at: number) {
  return new Intl.DateTimeFormat("en-US", interval === "hour"
    ? { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", timeZone: timezone }
    : { weekday: "short", month: "short", day: "numeric", timeZone: timezone }).format(at);
}

function PartialNote({ days }: { days: ReadonlyArray<PartialDay> }) {
  const gaps = days.filter((day) => day.reason === "gap").map((day) => day.day);
  const late = days.filter((day) => day.reason === "late").map((day) => day.day);
  return (
    <p className="text-xs text-muted-foreground" data-analytics-partial={days.map((day) => day.day).join(" ")}>
      Dashed days are partial.
      {gaps.length > 0 ? ` Some visits on ${gaps.join(", ")} could not be counted.` : ""}
      {late.length > 0 ? ` Visits for ${late.join(", ")} may still be arriving.` : ""}
      {days.some((day) => day.reason === "today") ? " Today is still arriving." : ""}
    </p>
  );
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

function BreakdownCard({ title, empty, children }: { title: string; empty: boolean; children: React.ReactNode }) {
  return (
    <Card size="sm" className="min-w-0 gap-2 px-3" data-analytics-table={title}>
      <h3 className="text-sm font-medium">{title}</h3>
      {empty ? <p className="py-4 text-center text-xs text-muted-foreground">None in this range</p> : (
        <Table className="table-fixed">{children}</Table>
      )}
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

function Gate({ title, children, action }: { title: string; children: React.ReactNode; action?: React.ReactNode }) {
  return (
    <Empty className="border" data-analytics-gate={title}>
      <EmptyHeader>
        <EmptyMedia variant="icon"><ChartLineIcon /></EmptyMedia>
        <EmptyTitle>{title}</EmptyTitle>
        <EmptyDescription>{children}</EmptyDescription>
      </EmptyHeader>
      {action ? <EmptyContent>{action}</EmptyContent> : null}
    </Empty>
  );
}

function Unpaid({ organizationSlug }: { organizationSlug: string }) {
  return (
    <Gate title="Analytics comes with Pro"
      action={<Button render={<Link to="/cloud/$organizationSlug/~/billing" params={{ organizationSlug }} />}>Upgrade</Button>}>
      Count visitors, pages and sources for every Environment, without cookies.
    </Gate>
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

function VisitsLost({ days }: { days: ReadonlyArray<string> }) {
  return (
    <Gate title="No visits counted">
      <span data-analytics-partial={days.join(" ")}>Some visits on {days.join(", ")} could not be counted.</span>
    </Gate>
  );
}

function NoVisitsYet() {
  return (
    <Gate title="No visits yet">
      Analytics is on. Numbers appear here after the first visits to this Environment&apos;s domains.
    </Gate>
  );
}
