import { keepPreviousData, useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { MetricChart, useMetricCursor } from "#/components/metrics/metric-chart";
import { Card } from "#/components/ui/card";
import { Skeleton } from "#/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { cn } from "#/lib/utils";
import { ClosedGateView } from "./analytics-gates";
import { formatCount } from "./analytics-model";
import { type AnalyticsScope, type ErrorsView as ErrorsViewData, errorsViewQueryOptions, type RequestRead } from "./analytics.queries";
import { RequestCard, RequestCardSkeleton, RequestControls, SampleTable } from "./request-controls";

type ErrorsViewProps = { scope: AnalyticsScope; read: RequestRead; onRead: (read: RequestRead) => void; toggle: ReactNode };

/** Failed requests per hour, which routes return which status, and the newest failures. */
export function ErrorsView({ scope, read, onRead, toggle }: ErrorsViewProps) {
  const { data: view, isPlaceholderData } = useQuery({ ...errorsViewQueryOptions(scope, read), placeholderData: keepPreviousData });
  if (view !== undefined && view.gate !== "on") return <ClosedGateView gate={view.gate} scope={scope} />;
  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        {toggle}
        <RequestControls read={read} services={view?.services} clamped={view?.clamped} onRead={onRead} />
      </div>
      <div className={cn("flex min-w-0 flex-col gap-4 transition-opacity", isPlaceholderData && "opacity-50")}>
        {view === undefined ? <ErrorsChartSkeleton /> : <ErrorsChart view={view} />}
        {view === undefined ? <RequestCardSkeleton title="By route" /> : <ByRoute rows={view.byRoute} />}
        {view === undefined ? <RequestCardSkeleton title="Newest" /> : (
          <RequestCard title="Newest" empty={view.newest.length === 0}>
            <SampleTable samples={view.newest} timezone={view.timezone} />
          </RequestCard>
        )}
      </div>
    </div>
  );
}

type OnErrors = Extract<ErrorsViewData, { gate: "on" }>;

function ErrorsChart({ view }: { view: OnErrors }) {
  const cursor = useMetricCursor();
  const total = view.hourly.reduce((sum, bucket) => sum + bucket.clientErrors + bucket.serverErrors, 0);
  const byTime = new Map(view.hourly.map((bucket) => [bucket.at, bucket]));
  const time = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", timeZone: view.timezone });
  return (
    <MetricChart id="analytics-errors" title="Errors" subtitle="per hour" value={formatCount(total)}
      times={view.hourly.map((bucket) => bucket.at)}
      series={[
        { label: "4xx", color: "var(--chart-4)", values: view.hourly.map((bucket) => bucket.clientErrors) },
        { label: "5xx", color: "var(--chart-5)", values: view.hourly.map((bucket) => bucket.serverErrors) },
      ]}
      format={formatCount} deploys={[]} from={view.range.start} to={view.range.end} cursor={cursor} height={180}
      renderTooltip={(at) => {
        const bucket = byTime.get(at);
        if (bucket === undefined) return null;
        return (
          <div className="flex flex-col gap-0.5">
            <span>{time.format(at)}</span>
            <span>4xx {formatCount(bucket.clientErrors)}</span>
            <span>5xx {formatCount(bucket.serverErrors)}</span>
          </div>
        );
      }} />
  );
}

function ErrorsChartSkeleton() {
  return (
    <Card size="sm" className="gap-2 px-3" aria-busy>
      <div className="flex items-baseline gap-2">
        <h3 className="text-sm font-medium">Errors</h3>
        <span className="text-xs text-muted-foreground">per hour</span>
      </div>
      <Skeleton className="h-[180px]" />
    </Card>
  );
}

function ByRoute({ rows }: { rows: OnErrors["byRoute"] }) {
  return (
    <RequestCard title="By route" empty={rows.length === 0}>
      <Table className="table-fixed">
        <TableHeader>
          <TableRow>
            <TableHead>Route</TableHead>
            <TableHead className="w-16 text-right">Status</TableHead>
            <TableHead className="w-24 text-right">Requests</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((row) => (
            <TableRow key={`${row.route}:${row.status}`}>
              <TableCell className="truncate font-mono" title={row.route}>{row.route}</TableCell>
              <TableCell className={cn("text-right font-mono tabular-nums", row.status >= 500 && "text-destructive")}>{row.status}</TableCell>
              <TableCell className="text-right font-mono tabular-nums">{formatCount(row.requests)}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </RequestCard>
  );
}
