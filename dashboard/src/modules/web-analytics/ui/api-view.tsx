import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ArrowDownIcon, ArrowUpIcon } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { cn } from "#/lib/utils";
import { ClosedGateView } from "./analytics-gates";
import {
  DEFAULT_ROUTE_SORT,
  formatCount,
  formatMs,
  formatRate,
  type RouteSort,
  type RouteSortKey,
  sortRoutes,
} from "./analytics-model";
import { type AnalyticsScope, type ApiView as ApiViewData, apiViewQueryOptions, type RequestRead } from "./analytics.queries";
import { RequestCard, RequestCardSkeleton, RequestControls, SampleTable } from "./request-controls";

type ApiViewProps = { scope: AnalyticsScope; read: RequestRead; onRead: (read: RequestRead) => void; toggle: ReactNode };

/** Every route's traffic, failures and latency over the window, and the slowest requests in it. */
export function ApiView({ scope, read, onRead, toggle }: ApiViewProps) {
  const { data: view, isPlaceholderData } = useQuery({ ...apiViewQueryOptions(scope, read), placeholderData: keepPreviousData });
  if (view !== undefined && view.gate !== "on") return <ClosedGateView gate={view.gate} scope={scope} />;
  return (
    <div className="flex min-w-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        {toggle}
        <RequestControls read={read} services={view?.services} clamped={view?.clamped} onRead={onRead} />
      </div>
      <div className={cn("flex min-w-0 flex-col gap-4 transition-opacity", isPlaceholderData && "opacity-50")}>
        {view === undefined ? <RequestCardSkeleton title="Routes" /> : <Routes routes={view.routes} />}
        {view === undefined ? <RequestCardSkeleton title="Slowest" /> : (
          <RequestCard title="Slowest" empty={view.slowest.length === 0}>
            <SampleTable samples={view.slowest} timezone={view.timezone} />
          </RequestCard>
        )}
      </div>
    </div>
  );
}

type RouteRows = Extract<ApiViewData, { gate: "on" }>["routes"];

const COLUMNS: ReadonlyArray<{ key: RouteSortKey; label: string; className?: string }> = [
  { key: "route", label: "Route" },
  { key: "requests", label: "Requests", className: "w-24" },
  { key: "errorRate", label: "Errors", className: "w-20" },
  { key: "p50", label: "p50", className: "w-20" },
  { key: "p95", label: "p95", className: "w-20" },
  { key: "p99", label: "p99", className: "w-20" },
];

function Routes({ routes }: { routes: RouteRows }) {
  const [sort, setSort] = useState<RouteSort>(DEFAULT_ROUTE_SORT);
  const sorted = sortRoutes(routes, sort);
  return (
    <RequestCard title="Routes" empty={routes.length === 0}>
      <Table className="table-fixed">
        <TableHeader>
          <TableRow>
            {COLUMNS.map((column) => (
              <SortHead key={column.key} column={column} sort={sort}
                onSort={(key) => setSort((current) => current.key === key
                  ? { key, descending: !current.descending }
                  : { key, descending: key !== "route" })} />
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {sorted.map((row) => (
            <TableRow key={row.route}>
              <TableCell className="truncate font-mono" title={row.route}>{row.route}</TableCell>
              <TableCell className="text-right font-mono tabular-nums">{formatCount(row.requests)}</TableCell>
              <TableCell className={cn("text-right font-mono tabular-nums", row.errors > 0 && "text-destructive")}>
                {formatRate(row.errors, row.requests)}
              </TableCell>
              <TableCell className="text-right font-mono tabular-nums">{formatMs(row.p50)}</TableCell>
              <TableCell className="text-right font-mono tabular-nums">{formatMs(row.p95)}</TableCell>
              <TableCell className="text-right font-mono tabular-nums">{formatMs(row.p99)}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </RequestCard>
  );
}

function SortHead({ column, sort, onSort }: {
  column: (typeof COLUMNS)[number];
  sort: RouteSort;
  onSort: (key: RouteSortKey) => void;
}) {
  const active = sort.key === column.key;
  const Arrow = sort.descending ? ArrowDownIcon : ArrowUpIcon;
  return (
    <TableHead className={cn(column.className, column.key !== "route" && "text-right")}
      aria-sort={active ? (sort.descending ? "descending" : "ascending") : "none"}>
      <button type="button" className={cn("inline-flex items-center gap-1 hover:text-foreground", active && "text-foreground")}
        onClick={() => onSort(column.key)}>
        {column.label}
        {active ? <Arrow className="size-3" /> : null}
      </button>
    </TableHead>
  );
}
