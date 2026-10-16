import type { ReactNode } from "react";
import { Card } from "#/components/ui/card";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Skeleton } from "#/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { cn } from "#/lib/utils";
import { formatMs, REQUEST_WINDOWS } from "./analytics-model";
import type { RequestRead } from "./analytics.queries";

export type RequestControlsProps = {
  read: RequestRead;
  services: ReadonlyArray<string> | undefined;
  clamped: boolean | undefined;
  onRead: (read: RequestRead) => void;
};

/** The window and the service the API and Errors views read. */
export function RequestControls({ read, services, clamped, onRead }: RequestControlsProps) {
  const known = services ?? [];
  const items = [
    { value: null, label: "All services" },
    ...[...new Set(read.service === undefined ? known : [...known, read.service])].map((service) => ({ value: service, label: service })),
  ];
  return (
    <>
      <ToggleGroup variant="outline" size="sm" spacing={0} value={[read.window]} aria-label="Window"
        onValueChange={([next]) => {
          const window = REQUEST_WINDOWS.find((option) => option.spec === next)?.spec;
          if (window !== undefined) onRead({ ...read, window });
        }}>
        {REQUEST_WINDOWS.map((option) => <ToggleGroupItem key={option.spec} value={option.spec}>{option.spec}</ToggleGroupItem>)}
      </ToggleGroup>
      <Select items={items} value={read.service ?? null} onValueChange={(next) => onRead({ ...read, service: next ?? undefined })}>
        <SelectTrigger size="sm" aria-label="Service"><SelectValue /></SelectTrigger>
        <SelectContent>
          {items.map((item) => <SelectItem key={item.value} value={item.value}>{item.label}</SelectItem>)}
        </SelectContent>
      </Select>
      {clamped ? <span className="text-xs text-muted-foreground">Starts at the oldest day kept</span> : null}
    </>
  );
}

export function RequestCard({ title, empty, children, className }: { title: string; empty: boolean; children: ReactNode; className?: string }) {
  return (
    <Card size="sm" className={cn("min-w-0 gap-2 px-3", className)} data-analytics-table={title}>
      <h3 className="text-sm font-medium">{title}</h3>
      {empty ? <p className="py-4 text-center text-xs text-muted-foreground">None in this range</p> : children}
    </Card>
  );
}

export function RequestCardSkeleton({ title }: { title: string }) {
  return (
    <Card size="sm" className="min-w-0 gap-2 px-3" aria-busy>
      <h3 className="text-sm font-medium">{title}</h3>
      <div className="flex flex-col gap-2 py-1">
        {[0, 1, 2, 3].map((row) => <Skeleton key={row} className="h-5" />)}
      </div>
    </Card>
  );
}

export type Sample = {
  readonly requestId: string;
  readonly ts: number;
  readonly service: string;
  readonly method: string;
  readonly path: string;
  readonly status: number;
  readonly durationMs: number;
};

export function SampleTable({ samples, timezone }: { samples: ReadonlyArray<Sample>; timezone: string }) {
  const time = new Intl.DateTimeFormat("en-US", {
    month: "short", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit", timeZone: timezone,
  });
  return (
    <Table className="table-fixed">
      <TableHeader>
        <TableRow>
          <TableHead className="w-36">Time</TableHead>
          <TableHead className="w-16">Method</TableHead>
          <TableHead>Path</TableHead>
          <TableHead className="w-16 text-right">Status</TableHead>
          <TableHead className="w-20 text-right">Duration</TableHead>
          <TableHead className="w-44">Request ID</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {samples.map((sample) => (
          <TableRow key={`${sample.requestId}:${sample.ts}`}>
            <TableCell className="tabular-nums text-muted-foreground">{time.format(sample.ts)}</TableCell>
            <TableCell className="font-mono">{sample.method}</TableCell>
            <TableCell className="truncate font-mono" title={sample.path}>{sample.path}</TableCell>
            <TableCell className={cn("text-right font-mono tabular-nums", sample.status >= 500 && "text-destructive")}>{sample.status}</TableCell>
            <TableCell className="text-right font-mono tabular-nums">{formatMs(sample.durationMs)}</TableCell>
            <TableCell className="truncate font-mono text-muted-foreground select-all" title={sample.requestId}>{sample.requestId}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
