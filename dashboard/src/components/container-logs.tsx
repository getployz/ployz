import { useState, useSyncExternalStore } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { useMutation } from "@tanstack/react-query";
import { useLogScroll } from "./log-scroll";
import { Schema } from "effect";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { mergeContainerHistory, remainingHistory, containerLogPageSchema, type ContainerLogRow } from "#/modules/runtime/container-log.collection";
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
  const { collection, query, refresh } = stream;
  const { data: loaded = [] } = useLiveQuery({ queryKey: ["container-logs", collection.id], query: q => q.from({ log: collection }), gcTime: 100 });
  const { status, errors } = useSyncExternalStore(stream.subscribe, stream.getSnapshot, stream.getSnapshot);
  const [search, setSearch] = useState("");
  const [machine, setMachine] = useState("");
  const [service, setService] = useState("");
  const [exhausted, setExhausted] = useState<Record<string, string>>({});
  const rows = [...loaded, ...lifecycle].filter(row => (!machine || row.machineId === machine) && (!service || row.serviceName === service) && row.message.toLowerCase().includes(search.toLowerCase())).sort((a, b) => {
    const difference = BigInt(a.timestamp) - BigInt(b.timestamp);
    return difference < 0n ? -1 : difference > 0n ? 1 : a.id.localeCompare(b.id);
  });
  const { element, virtual } = useLogScroll({ count: rows.length, getItemKey: index => rows[index]?.id ?? index });
  const history = useMutation({
    mutationFn: async () => {
      const before = remainingHistory(loaded, exhausted);
      const response = await fetch(`/api/runtime/logs?${query}&before=${encodeURIComponent(JSON.stringify(before))}`, { signal: stream.signal });
      if (!response.ok) throw new Error("Could not load older logs.");
      const page = Schema.decodeUnknownSync(containerLogPageSchema)(await response.json());
      return { page, before };
    },
    onSuccess: ({ page, before }) => {
      mergeContainerHistory(collection, page.records);
      stream.setErrors(Object.fromEntries(page.errors.map(error => [`${error.machineId}/${error.containerId}`, error.message])));
      setExhausted(previous => {
        const next = { ...previous };
        for (const [source, boundary] of Object.entries(before)) {
          const failed = page.errors.some(error => `${error.machineId}/${error.containerId}` === source);
          const progressed = page.records.some(row => `${row.machineId}/${row.containerId}` === source && BigInt(row.timestamp) < BigInt(boundary));
          if (!failed && !progressed) next[source] = boundary;
        }
        return next;
      });
    },
  });
  const machines = new Map(loaded.map(row => [row.machineId, row.machineName]));
  const services = [...new Set(loaded.map(row => row.serviceName))];
  return <div className="flex min-h-0 flex-col gap-3">
    <div className="flex flex-wrap items-center gap-2">
      <Input aria-label="Search loaded logs" placeholder="Search loaded logs" value={search} onChange={event => setSearch(event.target.value)} className="min-w-40 flex-1" />
      <LogFilter label="All services" value={service} onChange={setService} options={services.map(name => [name, name])} />
      <LogFilter label="All servers" value={machine} onChange={setMachine} options={[...machines]} />
      <Button variant="ghost" size="sm" onClick={refresh}>Refresh</Button>
    </div>
    <div className="flex items-center justify-between gap-2">
      <Button variant="outline" size="sm" disabled={history.isPending || !Object.keys(remainingHistory(loaded, exhausted)).length} onClick={() => history.mutate()}>{history.isPending ? "Loading…" : "Load older"}</Button>
      <span role="status" className="text-xs text-muted-foreground">{status}</span>
      <Button variant="ghost" size="sm" onClick={() => virtual.scrollToEnd()}>Latest</Button>
    </div>
    {history.error ? <p role="alert">Could not load older logs.</p> : null}
    {Object.entries(errors).map(([source, message]) => <p role="alert" key={source}>{source}: {message}</p>)}
    <div ref={element} tabIndex={0} aria-label="Container logs" className="h-80 overflow-auto font-mono text-xs">
      {!rows.length ? <p className="text-muted-foreground">No matching output available.</p> : null}
      <div className="relative w-full" style={{ height: virtual.getTotalSize() }}>
        {virtual.getVirtualItems().map(item => {
          const row = rows[item.index];
          if (!row) return null;
          return <div key={item.key} ref={virtual.measureElement} data-index={item.index} className="absolute left-0 top-0 w-full whitespace-pre-wrap break-words leading-6" style={{ transform: `translateY(${item.start}px)` }}>
            <time className="mr-3 text-muted-foreground">{new Date(Number(BigInt(row.timestamp) / 1_000_000n)).toISOString().slice(11, 23)}Z</time>
            <span className="mr-3 text-muted-foreground">{row.serviceName} · {row.machineName}</span>{row.message}
          </div>;
        })}
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
