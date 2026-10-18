import { useVirtualizer } from "@tanstack/react-virtual";
import { createContext, memo, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { Button } from "#/components/ui/button";
import { Card, CardContent } from "#/components/ui/card";
import { Checkbox } from "#/components/ui/checkbox";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { Separator } from "#/components/ui/separator";
import { useMediaQuery } from "#/hooks/use-media-query";
import { cn } from "#/lib/utils";
import { ENROLLMENT_STAGES } from "#/modules/machines/waiting/server-enrollment";
import { SELECTION_MAX } from "#/modules/machines/waiting/probe";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { EnrollSheet } from "./enroll-sheet";
import { EnrollingList, enrollingRows, STAGES } from "./enrolling-list";
import { useCancelEnrollment, useEnrollmentStatus, useRejectWaiting, useSettledProbe, useWaitingServerDetail, useWaitingServers } from "./use-waiting";
import { WaitingDetailSheet } from "./waiting-detail-sheet";
import {
  hostnameOrIpMatcher,
  providerOf,
  READINESS_FILTERS,
  readiness,
  type Readiness,
  type ReadinessFilter,
  selectable,
  specText,
  statusCell,
} from "./waiting-row";

const WaitingStatusContext = createContext<{ servers: readonly WaitingServerView[]; now: number }>({ servers: [], now: 0 });

export const ROW_PX = 52;
// Below sm a row stacks its specs under the name, so it needs a third line.
const STACKED_ROW_PX = 68;
export const VISIBLE_ROWS = 10;
const GRID = "grid grid-cols-[20px_minmax(0,1fr)_auto] content-center items-center gap-x-3 gap-y-0.5 px-3 sm:grid-cols-[20px_minmax(0,1fr)_150px_170px_200px]";

/** The readiness mix, in the order a server moves through it, with the colour of its bar segment. */
const MIX = [
  { value: "ready", bar: "bg-success", text: "text-success" },
  { value: "preparing", bar: "bg-info", text: "text-info" },
  { value: "blocked", bar: "bg-warning", text: "text-warning" },
  { value: "offline", bar: "bg-destructive", text: "text-destructive" },
] as const satisfies readonly { value: Readiness; bar: string; text: string }[];

/** Servers waiting to join and servers enrolling, in one tray. The page renders it only when it holds something. */
export function IntakeTray({ organizationSlug, token, firstRun }: { organizationSlug: string; token: string | undefined; firstRun: boolean }) {
  const waiting = useWaitingServers(organizationSlug);
  const enrollments = useEnrollmentStatus(organizationSlug);
  const cancel = useCancelEnrollment(organizationSlug);
  const servers = waiting.data ?? [];
  const attempts = enrollments.data?.attempts ?? [];
  const enrolling = enrollingRows(attempts).length > 0;
  return (
    <Card>
      <CardContent className="flex flex-col gap-4">
        {/* Stays mounted while the last waiting server enrolls, so its sheet survives the row leaving. */}
        <WaitingList organizationSlug={organizationSlug} servers={servers} now={waiting.dataUpdatedAt} token={token} hasFirst={enrollments.data?.hasFirst ?? true} firstRun={firstRun} />
        {servers.length > 0 && enrolling ? <Separator /> : null}
        <EnrollingList attempts={attempts} onCancel={cancel.mutate} />
      </CardContent>
    </Card>
  );
}

export function WaitingList({ organizationSlug, servers, now, token, hasFirst = true, firstRun = false }: {
  organizationSlug: string;
  servers: readonly WaitingServerView[];
  now: number;
  token?: string | undefined;
  hasFirst?: boolean;
  firstRun?: boolean;
}) {
  const [filter, setFilter] = useState<ReadinessFilter>("all");
  const [provider, setProvider] = useState("");
  const [tokenFilter, setTokenFilter] = useState(token ?? "");
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [openId, setOpenId] = useState<string | null>(null);
  const [enroll, setEnroll] = useState<readonly WaitingServerView[] | null>(null);
  const only = servers.length === 1 ? servers[0] : undefined;
  // Filters earn their place only once the list overflows its box; below that every server is shown.
  const bulk = servers.length > VISIBLE_ROWS;
  const arrived = firstRun && only && readiness(only) === "ready" ? only : undefined;
  const ordered = useMemo(() => [...servers].sort((a, b) => a.arrivalAt.localeCompare(b.arrivalAt) || a.firstSeenAt.localeCompare(b.firstSeenAt) || a.id.localeCompare(b.id)), [servers]);
  const statusContext = useMemo(() => ({ servers, now }), [servers, now]);
  const reject = useRejectWaiting(organizationSlug);

  const counts = new Map<ReadinessFilter, number>([["all", servers.length]]);
  for (const server of servers) counts.set(readiness(server), (counts.get(readiness(server)) ?? 0) + 1);
  const providers = [...new Set(servers.map(providerOf))].sort();
  const tokens = [...new Set(servers.map((server) => server.fleetToken))].sort();
  const shownToken = tokens.includes(tokenFilter) ? tokenFilter : "";
  const matches = hostnameOrIpMatcher(query);
  const rows = !bulk ? ordered : ordered.filter((server) =>
    (filter === "all" || readiness(server) === filter)
    && (provider === "" || providerOf(server) === provider)
    && (shownToken === "" || server.fleetToken === shownToken)
    && matches(server));
  const chosen = rows.filter((server) => picked.has(server.id) && selectable(server)).map((server) => server.id);
  const pickedIds = new Set(chosen);
  const full = chosen.length >= SELECTION_MAX;
  const pickable = rows.filter(selectable);
  const allPicked = pickable.length > 0 && pickable.slice(0, SELECTION_MAX).every((server) => pickedIds.has(server.id));
  const detail = useWaitingServerDetail(organizationSlug, openId);
  const scroller = useRef<HTMLDivElement>(null);
  const rowPx = useMediaQuery("(max-width: 639px)") ? STACKED_ROW_PX : ROW_PX;
  const boxRect = { width: 0, height: rowPx * VISIBLE_ROWS };
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroller.current,
    getItemKey: (index) => rows[index]?.id ?? index,
    estimateSize: () => rowPx,
    // The box is ten rows tall by construction, so its height is known rather than measured.
    initialRect: boxRect,
    observeElementRect: (_, report) => report(boxRect),
    overscan: 2,
  });
  const items = virtual.getVirtualItems();
  const padTop = items[0]?.start ?? 0;
  const padBottom = virtual.getTotalSize() - (items.at(-1)?.end ?? 0);

  useSettledProbe(organizationSlug, openId === null ? chosen : [openId]);

  const toggle = useCallback((id: string, on: boolean) => {
    setPicked((current) => {
      const next = new Set(current);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);
  const rejectIds = (ids: readonly string[]) => {
    reject.mutate(ids);
    setPicked(new Set());
    setOpenId(null);
  };

  const sheets = <>
    {enroll ? <EnrollSheet organizationSlug={organizationSlug} servers={enroll} hasFirst={hasFirst} onClose={() => { setEnroll(null); setPicked(new Set()); }} /> : null}
    <WaitingDetailSheet
      open={openId !== null}
      failed={detail.isError}
      server={openId === null ? null : detail.data ?? null}
      now={now}
      onOpenChange={(next) => { if (!next) setOpenId(null); }}
      onReject={(id) => rejectIds([id])}
    />
  </>;
  if (servers.length === 0) return sheets;
  const mix = MIX.filter((part) => (counts.get(part.value) ?? 0) > 0);

  return (
    <WaitingStatusContext value={statusContext}><section aria-labelledby="waiting-heading" className="flex flex-col gap-2.5">
      {arrived === undefined ? (
        <div className="flex flex-col gap-2">
          <div className="flex flex-wrap items-center gap-2.5">
            <h2 id="waiting-heading" className="text-sm font-semibold">Waiting to join</h2>
            <span className="text-xs text-muted-foreground tabular-nums">{servers.length}</span>
            <span className="flex items-center gap-2.5 text-xs tabular-nums">
              {mix.map((part) => <span key={part.value} className={part.text}>{counts.get(part.value)} {part.value}</span>)}
            </span>
          </div>
          <div aria-hidden="true" className="flex h-1 gap-0.5 overflow-hidden rounded-full">
            {mix.map((part) => <span key={part.value} data-mix={part.value} className={part.bar} style={{ flex: counts.get(part.value) }} />)}
          </div>
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-2.5">
          <h2 id="waiting-heading" className="text-sm font-semibold">{arrived.report.hostname} is ready to enroll</h2>
          <div className="ml-auto flex items-center gap-2">
            <Button size="sm" variant="outline" onClick={() => rejectIds([arrived.id])}>Reject</Button>
            <Button size="sm" variant="ink" onClick={() => setEnroll([arrived])}>Enroll</Button>
          </div>
        </div>
      )}
      {bulk ? <div className="flex flex-wrap items-center gap-2.5">
        <div className="flex flex-wrap items-center gap-1">
          <div role="group" aria-label="Readiness" className="flex items-center gap-1">
            {READINESS_FILTERS.map((option) => (
              <button
                key={option.value}
                type="button"
                aria-pressed={filter === option.value}
                onClick={() => setFilter(option.value)}
                className="h-7 rounded-md border border-transparent px-2.5 text-[13px] text-muted-foreground aria-pressed:border-border aria-pressed:bg-card aria-pressed:text-foreground"
              >
                {option.label} <span className="text-muted-foreground tabular-nums">{counts.get(option.value) ?? 0}</span>
              </button>
            ))}
          </div>
          <NativeSelect size="sm" aria-label="Provider" value={provider} onChange={(event) => setProvider(event.target.value)}>
            <NativeSelectOption value="">All providers</NativeSelectOption>
            {providers.map((name) => <NativeSelectOption key={name} value={name}>{name}</NativeSelectOption>)}
          </NativeSelect>
          <NativeSelect size="sm" aria-label="Token" value={shownToken} onChange={(event) => setTokenFilter(event.target.value)}>
            <NativeSelectOption value="">All tokens</NativeSelectOption>
            {tokens.map((name) => <NativeSelectOption key={name} value={name}>{name}</NativeSelectOption>)}
          </NativeSelect>
          <Input
            aria-label="Filter"
            placeholder="Filter, or hz-fsn1-*"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            className="h-7 w-40 text-[13px]"
          />
        </div>
      </div> : null}
      <div className="overflow-x-auto rounded-lg border">
        <div role="table" aria-label="Waiting servers" className="text-[13px] sm:min-w-[680px]">
          <div role="row" className={cn(GRID, "h-9 text-xs text-muted-foreground")}>
            <span role="columnheader">
              <Checkbox
                aria-label="Select all"
                checked={allPicked}
                disabled={pickable.length === 0}
                onCheckedChange={(on) => setPicked(on ? new Set(pickable.slice(0, SELECTION_MAX).map((server) => server.id)) : new Set())}
              />
            </span>
            <span role="columnheader">Name</span>
            <span role="columnheader" className="hidden sm:block">IP</span>
            <span role="columnheader" className="hidden sm:block">CPU · RAM · disk</span>
            <span role="columnheader">Status</span>
          </div>
          <div ref={scroller} role="rowgroup" data-slot="waiting-box" className="overflow-y-auto" style={{ maxHeight: rowPx * VISIBLE_ROWS }}>
            {rows.length === 0 ? (
              <div role="row" className={cn(GRID, "border-t text-muted-foreground")} style={{ height: rowPx }}>
                <span />
                <span role="cell">Nothing matches</span>
              </div>
            ) : (
              <div style={{ paddingTop: padTop, paddingBottom: padBottom }}>
                {items.map((item) => {
                  const server = rows[item.index];
                  return server === undefined ? null : (
                    <WaitingRow
                      key={server.id}
                      server={server}
                      picked={pickedIds.has(server.id)}
                      full={full}
                      height={rowPx}
                      onPick={toggle}
                      onOpen={setOpenId}
                    />
                  );
                })}
              </div>
            )}
          </div>
        </div>
      </div>
      {chosen.length > 0 ? (
        <div className="fixed bottom-6 left-1/2 z-40 flex -translate-x-1/2 items-center gap-3 rounded-xl bg-primary py-2 pr-2 pl-4 text-sm text-primary-foreground shadow-lg">
          <span className="tabular-nums">{chosen.length} selected</span>
          {full ? <span className="text-primary-foreground/70">{SELECTION_MAX} max</span> : null}
          <Button size="sm" variant="ghost" className="text-primary-foreground" onClick={() => setPicked(new Set())}>Clear</Button>
          <Button size="sm" variant="destructive" onClick={() => rejectIds(chosen)}>Reject</Button>
          <Button size="sm" variant="secondary" onClick={() => setEnroll(rows.filter((row) => pickedIds.has(row.id)))}>Enroll {chosen.length}</Button>
        </div>
      ) : null}
      {sheets}
    </section></WaitingStatusContext>
  );
}

const WaitingRow = memo(function WaitingRow({ server, picked, full, height, onPick, onOpen }: {
  server: WaitingServerView;
  picked: boolean;
  full: boolean;
  height: number;
  onPick: (id: string, on: boolean) => void;
  onOpen: (id: string) => void;
}) {
  const { report } = server;
  const state = readiness(server);
  return (
    <div
      role="row"
      data-state={state}
      aria-selected={picked}
      onClick={() => onOpen(server.id)}
      className={cn(GRID, "cursor-pointer border-t hover:bg-muted/50", picked && "bg-muted")}
      style={{ height }}
    >
      <span role="cell" onClick={(event) => event.stopPropagation()}>
        <Checkbox
          aria-label={`Select ${report.hostname}`}
          checked={picked}
          disabled={!selectable(server) || (full && !picked)}
          onCheckedChange={(on) => onPick(server.id, on)}
        />
      </span>
      <TwoLineCell>
        <button type="button" className="truncate text-left font-medium outline-none focus-visible:underline" onClick={(event) => { event.stopPropagation(); onOpen(server.id); }}>
          {report.hostname}
        </button>
        <span>{report.os.id} {report.os.version}</span>
      </TwoLineCell>
      <TwoLineCell className="hidden sm:flex">
        <span className="font-mono">{server.observedIp}</span>
        <span>{providerOf(server)}</span>
      </TwoLineCell>
      <span role="cell" className="col-span-2 col-start-2 row-start-2 truncate text-muted-foreground sm:col-auto sm:row-auto">{specText(report)}</span>
      <WaitingStatus id={server.id} />
    </div>
  );
}, (a, b) => a.server.id === b.server.id && a.server.report === b.server.report
  && a.server.observedIp === b.server.observedIp && a.server.status === b.server.status
  && a.server.returned === b.server.returned && a.picked === b.picked && a.full === b.full
  && a.onPick === b.onPick && a.onOpen === b.onOpen);

function WaitingStatus({ id }: { id: string }) {
  const { servers, now } = useContext(WaitingStatusContext);
  const server = servers.find((row) => row.id === id);
  if (server === undefined) return null;
  const status = statusCell(server, now);
  const returned = server.returned;
  const stage = ENROLLMENT_STAGES.find((stage) => stage === returned?.stage);
  const reason = returned === null ? null : returned.state === "cancelled" ? "Cancelled"
    : stage === undefined ? "Failed" : `Failed at ${STAGES[stage]}`;
  const detail = reason === null ? status.detail : `${status.detail} · ${reason}${returned?.message ? ` · ${returned.message}` : ""}`;
  return <TwoLineCell>
    <span className={cn("font-medium", readiness(server) === "blocked" && "text-destructive")}>{status.bold}</span>
    <span title={detail}>{detail}</span>
  </TwoLineCell>;
}

function TwoLineCell({ children, className }: { children: [ReactNode, ReactNode]; className?: string }) {
  const [first, second] = children;
  return (
    <div role="cell" className={cn("flex min-w-0 flex-col leading-tight", className)}>
      <span className="truncate">{first}</span>
      <span className="truncate text-xs text-muted-foreground">{second}</span>
    </div>
  );
}
