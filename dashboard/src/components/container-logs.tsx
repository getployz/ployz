import { useRef, useState, useSyncExternalStore } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { Link } from "@tanstack/react-router";
import { LatestButton, LOG_TIME_COLUMN, LogEmpty, LogHeader, LogSkeleton, useLogScroll } from "./log-scroll";
import { cn } from "#/lib/utils";
import { logTimestamp, useTimeZone } from "#/utils/time-zone";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectGroup, SelectItem } from "#/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { compareLogRows, type ContainerLogRow } from "#/modules/runtime/container-log.collection";

import { getContainerLogStream, type ContainerLogSelection } from "#/modules/runtime/container-log.stream";
export type { ContainerLogSelection } from "#/modules/runtime/container-log.stream";

const LEVELS = ["error", "warn", "info", "debug"] as const;
const levelTone = { error: "text-destructive", warn: "text-warning", info: null, debug: "text-muted-foreground" } as const;
const gapReasons = {
  not_captured: "Logs from this stretch weren’t captured.",
  corrupt: "Some of this stretch’s log data was unreadable, so it was skipped.",
} as const;

export function ContainerLogs({ selection }: { selection: ContainerLogSelection }) {
  const scope = useCollectionScope();
  const key = JSON.stringify([scope.sessionId, scope.userId, selection]);
  return <LogViewer key={key} selection={selection} />;
}

function LogViewer({ selection }: { selection: ContainerLogSelection }) {
  const scope = useCollectionScope();
  const stream = getContainerLogStream(selection, scope);
  const { collection } = stream;
  const { data: loaded = [] } = useLiveQuery({ queryKey: ["container-logs", collection.id], query: q => q.from({ log: collection }), gcTime: 100 });
  const { opened, offline, refused, missing, historyPending, historyError } = useSyncExternalStore(stream.subscribe, stream.getSnapshot, stream.getSnapshot);
  const timestamp = logTimestamp(useTimeZone());
  const [search, setSearch] = useState("");
  const [machine, setMachine] = useState("");
  const [service, setService] = useState("");
  const [levels, setLevels] = useState<readonly string[]>([]);
  // A gap is about every line, so it hides only behind a filter on what lines say.
  const matches = (row: ContainerLogRow) => row.kind === "gap" ? !search && !levels.length
    : (!levels.length || levels.includes(row.level)) && row.message.toLowerCase().includes(search.toLowerCase());
  const rows = loaded.filter(row => (!machine || row.machineId === machine) && (!service || row.serviceName === service) && matches(row)).sort(compareLogRows);
  const missingServers = [...missing.history, ...Object.entries(missing.live).filter(([id]) => !missing.history.some(server => server.machineId === id)).map(([machineId, server]) => ({ machineId, ...server }))];
  const touchY = useRef(0);
  const dragging = useRef(false);
  function loadAtTop(delta = 0) {
    if ((element.current?.scrollTop ?? 0) + delta < 160 && !historyError) void stream.loadOlder();
  }
  const { element, virtual } = useLogScroll({
    count: rows.length, getItemKey: index => rows[index]?.id ?? index,
    onChange: (instance, sync) => {
      // Lines landing push the end down before the view follows them there; only the reader's scroll stops following.
      if (sync || instance.isAtEnd()) stream.follow(instance.isAtEnd());
      if (dragging.current && sync && instance.scrollDirection === "backward" && (instance.scrollOffset ?? 0) < 160 && !historyError) void stream.loadOlder();
    },
  });
  const machines = new Map(loaded.map(row => [row.machineId, row.machineName]));
  const services = [...new Set(loaded.map(row => row.serviceName))];
  // A line names its service and server only where they vary; a filter shows only with something to pick.
  const servicesVary = !selection.serviceId && services.length > 1;
  const machinesVary = machines.size > 1;
  const offlineLink = <Link to="/cloud/$organizationSlug/~/servers" params={{ organizationSlug: selection.organizationSlug }} className="underline underline-offset-4">Check your servers</Link>;
  const empty = rows.length ? null
    : offline ? <LogEmpty title="Your servers are offline">Logs stream again once a server reconnects. {offlineLink}</LogEmpty>
    : refused ? <LogEmpty title="Couldn’t load logs">Trying again…</LogEmpty>
    : !opened || (historyPending && !loaded.length) ? <LogSkeleton label="Loading logs" time={LOG_TIME_COLUMN.container} />
    : loaded.length ? <LogEmpty title="No logs match your filters">{stream.hasOlder ? "Scroll up or press Home to check older logs." : null}</LogEmpty>
    : <LogEmpty title="No logs yet">Output shows up here as soon as the service writes any.</LogEmpty>;
  return <div className="flex min-h-0 grow flex-col gap-3">
    <div className="flex flex-wrap items-center gap-2">
      <Input aria-label="Search loaded logs" placeholder="Search loaded logs" value={search} onChange={event => setSearch(event.target.value)} className="min-w-40 flex-1" />
      {servicesVary ? <LogFilter label="All services" value={service} onChange={setService} options={services.map(name => [name, name])} /> : null}
      {machinesVary ? <LogFilter label="All servers" value={machine} onChange={setMachine} options={[...machines]} /> : null}
      <ToggleGroup multiple variant="outline" size="sm" spacing={0} aria-label="Levels" value={[...levels]} onValueChange={setLevels}>
        {LEVELS.map(level => <ToggleGroupItem key={level} value={level} className="capitalize">{level}</ToggleGroupItem>)}
      </ToggleGroup>
    </div>
    {offline && rows.length ? <p className="text-muted-foreground">Your servers are offline, so these are the latest logs they sent. {offlineLink}</p> : null}
    {refused && rows.length ? <p role="alert" className="text-muted-foreground">Couldn’t reach the log stream, so new lines are paused. Trying again…</p> : null}
    {missingServers.length ? <Alert role="alert">
      <AlertTitle>{missingServers.length === 1 ? "A server’s logs are missing" : `${missingServers.length} servers’ logs are missing`}</AlertTitle>
      <AlertDescription><ul>{missingServers.map(server => <li key={server.machineId}><span className="font-medium">{server.machineName}</span>: {server.message}</li>)}</ul></AlertDescription>
    </Alert> : null}
    <div className="flex min-h-0 flex-1 flex-col">
      <LogHeader time={LOG_TIME_COLUMN.container}>Message</LogHeader>
      {historyPending ? <LogSkeleton rows={1} label="Loading older logs" time={LOG_TIME_COLUMN.container} /> : null}
      {historyError ? <div role="alert" className="flex items-center gap-2"><span>Couldn’t load older logs.</span> <Button variant="ghost" size="sm" onClick={() => void stream.loadOlder()}>Retry</Button></div> : null}
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
            {virtual.getVirtualItems().map(item => {
              const row = rows[item.index];
              if (!row) return null;
              if (row.kind === "gap") {
                return <div key={item.key} ref={virtual.measureElement} data-index={item.index} role="note" className="absolute left-0 top-0 flex w-full gap-3 px-1 leading-6 text-muted-foreground italic max-sm:flex-col max-sm:gap-0" style={{ transform: `translateY(${item.start}px)` }}>
                  <time className={cn("shrink-0 max-sm:w-auto", LOG_TIME_COLUMN.container)}>{timestamp(new Date(Number(BigInt(row.timestamp) / 1_000_000n)))}</time>
                  <span className="min-w-0 flex-1">{servicesVary || machinesVary ? <span className="mr-3">{[servicesVary && row.serviceName, machinesVary && row.machineName].filter(Boolean).join(" · ")}</span> : null}{gapReasons[row.reason]}</span>
                </div>;
              }
              // A phone stacks the time over its line, which keeps the width.
              return <div key={item.key} ref={virtual.measureElement} data-index={item.index} className="absolute left-0 top-0 flex w-full gap-3 px-1 leading-6 max-sm:flex-col max-sm:gap-0" style={{ transform: `translateY(${item.start}px)` }}>
                <time className={cn("shrink-0 text-muted-foreground max-sm:w-auto", LOG_TIME_COLUMN.container)}>{timestamp(new Date(Number(BigInt(row.timestamp) / 1_000_000n)))}</time>
                <span className={cn("min-w-0 flex-1 whitespace-pre-wrap break-words", levelTone[row.level])}>
                  {servicesVary || machinesVary ? <span className="mr-3 text-muted-foreground">{[servicesVary && row.serviceName, machinesVary && row.machineName].filter(Boolean).join(" · ")}</span> : null}{row.message}
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
