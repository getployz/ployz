import { useRef, useState, useSyncExternalStore } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { Link } from "@tanstack/react-router";
import { LatestButton, LOG_TIME_COLUMN, LogEmpty, LogHeader, LogSkeleton, useLogScroll } from "./log-scroll";
import { cn } from "#/lib/utils";
import { logTimestamp, useTimeZone } from "#/utils/time-zone";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { type ContainerLogRow } from "#/modules/runtime/container-log.collection";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectGroup, SelectItem } from "#/components/ui/select";

import { getContainerLogStream, type ContainerLogSelection } from "#/modules/runtime/container-log.stream";
export type { ContainerLogSelection } from "#/modules/runtime/container-log.stream";

export function ContainerLogs({ selection, lifecycle = [] }: { selection: ContainerLogSelection; lifecycle?: readonly ContainerLogRow[] }) {
  const scope = useCollectionScope();
  const key = JSON.stringify([scope.sessionId, scope.userId, selection]);
  return <LogViewer key={key} selection={selection} lifecycle={lifecycle} />;
}

function LogViewer({ selection, lifecycle }: { selection: ContainerLogSelection; lifecycle: readonly ContainerLogRow[] }) {
  const scope = useCollectionScope();
  const stream = getContainerLogStream(selection, scope);
  const { collection } = stream;
  const { data: loaded = [] } = useLiveQuery({ queryKey: ["container-logs", collection.id], query: q => q.from({ log: collection }), gcTime: 100 });
  const { opened, offline, errors, historyPending, historyError } = useSyncExternalStore(stream.subscribe, stream.getSnapshot, stream.getSnapshot);
  const timestamp = logTimestamp(useTimeZone());
  const [search, setSearch] = useState("");
  const [machine, setMachine] = useState("");
  const [service, setService] = useState("");
  const all = [...loaded, ...lifecycle];
  const rows = all.filter(row => (!machine || row.machineId === machine) && (!service || row.serviceName === service) && row.message.toLowerCase().includes(search.toLowerCase())).sort((a, b) => {
    const difference = BigInt(a.timestamp) - BigInt(b.timestamp);
    return difference < 0n ? -1 : difference > 0n ? 1 : a.id.localeCompare(b.id);
  });
  const touchY = useRef(0);
  const dragging = useRef(false);
  function loadAtTop(delta = 0) {
    if ((element.current?.scrollTop ?? 0) + delta < 160 && !historyError) void stream.loadOlder();
  }
  const { element, virtual } = useLogScroll({
    paddingStart: 40, count: rows.length, getItemKey: index => rows[index]?.id ?? index,
    onChange: (instance, sync) => {
      if (dragging.current && sync && instance.scrollDirection === "backward" && (instance.scrollOffset ?? 0) < 160 && !historyError) void stream.loadOlder();
    },
  });
  const machines = new Map(loaded.map(row => [row.machineId, row.machineName]));
  const services = [...new Set(loaded.map(row => row.serviceName))];
  const offlineLink = <Link to="/cloud/$organizationSlug/~/servers" params={{ organizationSlug: selection.organizationSlug }} className="underline underline-offset-4">Check your servers</Link>;
  const empty = rows.length ? null
    : offline ? <LogEmpty title="Your servers are offline">Logs stream again once a server reconnects. {offlineLink}</LogEmpty>
    : !opened ? <LogSkeleton label="Loading logs" time={LOG_TIME_COLUMN.container} />
    : all.length ? <LogEmpty title="No logs match your filters" />
    : <LogEmpty title="No logs yet">Output shows up here as soon as the service writes any.</LogEmpty>;
  return <div className="flex min-h-0 grow flex-col gap-3">
    <div className="flex flex-wrap items-center gap-2">
      <Input aria-label="Search loaded logs" placeholder="Search loaded logs" value={search} onChange={event => setSearch(event.target.value)} className="min-w-40 flex-1" />
      {selection.serviceId ? null : <LogFilter label="All services" value={service} onChange={setService} options={services.map(name => [name, name])} />}
      <LogFilter label="All servers" value={machine} onChange={setMachine} options={[...machines]} />
    </div>
    {offline && rows.length ? <p className="text-muted-foreground">Your servers are offline, so these are the latest logs they sent. {offlineLink}</p> : null}
    {Object.entries(errors).map(([source, message]) => <p role="alert" key={source}>{source}: {message}</p>)}
    <div className="flex min-h-0 flex-1 flex-col">
      <LogHeader time={LOG_TIME_COLUMN.container}>Message</LogHeader>
      <div className="relative flex min-h-0 flex-1 flex-col">
        <div ref={element} role="region" tabIndex={0} aria-label="Container logs" className="flex min-h-0 flex-1 flex-col overflow-auto font-mono text-xs"
          onPointerDown={() => { dragging.current = true; }}
          onPointerUp={() => { dragging.current = false; }}
          onPointerLeave={() => { dragging.current = false; }}
          onWheel={event => { if (event.deltaY < 0) loadAtTop(event.deltaY); }}
          onKeyDown={event => { if (["ArrowUp", "PageUp", "Home"].includes(event.key)) loadAtTop(event.key === "Home" ? -Infinity : event.key === "PageUp" ? -event.currentTarget.clientHeight : -40); }}
          onTouchStart={event => { touchY.current = event.touches[0]?.clientY ?? 0; }}
          onTouchMove={event => {
            const next = event.touches[0]?.clientY ?? touchY.current;
            if (next > touchY.current) loadAtTop(touchY.current - next);
            touchY.current = next;
          }}>
          {empty ?? <div className="relative w-full shrink-0" style={{ height: virtual.getTotalSize() }}>
            <div className="absolute inset-x-0 top-0">
              {historyPending ? <LogSkeleton rows={1} label="Loading older logs" time={LOG_TIME_COLUMN.container} /> : null}
              {historyError ? <div role="alert" className="flex items-center gap-2"><span>Couldn’t load older logs.</span> <Button variant="ghost" size="sm" onClick={() => void stream.loadOlder()}>Retry</Button></div> : null}
            </div>
            {virtual.getVirtualItems().map(item => {
              const row = rows[item.index];
              if (!row) return null;
              return <div key={item.key} ref={virtual.measureElement} data-index={item.index} className="absolute left-0 top-0 flex w-full gap-3 px-1 leading-6" style={{ transform: `translateY(${item.start}px)` }}>
                <time className={cn("shrink-0 text-muted-foreground", LOG_TIME_COLUMN.container)}>{timestamp(new Date(Number(BigInt(row.timestamp) / 1_000_000n)))}</time>
                <span className={cn("min-w-0 flex-1 whitespace-pre-wrap break-words", row.channel === "stderr" && "text-destructive")}>
                  <span className="mr-3 text-muted-foreground">{row.serviceName} · {row.machineName}</span>{row.message}
                </span>
              </div>;
            })}
          </div>}
        </div>
        {rows.length > 0 && !virtual.isAtEnd() ? <LatestButton onClick={() => virtual.scrollToEnd()} /> : null}
      </div>
    </div>
  </div>;
}

function LogFilter({ label, value, onChange, options }: { label: string; value: string; onChange: (value: string) => void; options: [string, string][] }) {
  return <Select value={value} onValueChange={next => onChange(next ?? "")}>
    <SelectTrigger aria-label={label}><SelectValue>{options.find(([key]) => key === value)?.[1] ?? label}</SelectValue></SelectTrigger>
    <SelectContent><SelectGroup><SelectItem value="">{label}</SelectItem>{options.filter(([key]) => key).map(([key, name]) => <SelectItem key={key} value={key}>{name}</SelectItem>)}</SelectGroup></SelectContent>
  </Select>;
}
