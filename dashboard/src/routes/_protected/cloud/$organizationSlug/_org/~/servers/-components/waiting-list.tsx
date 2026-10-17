import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { cn } from "#/lib/utils";
import { SELECTION_MAX } from "#/modules/machines/waiting/probe";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import type { ServerEnrollmentView } from "#/modules/machines/waiting/enroll.server";
import { EnrollSheet } from "./enroll-sheet";
import { EnrollmentCounters, EnrollingList, waitingAfterReturns } from "./enrolling-list";
import { useCancelEnrollment, useEnrollmentStatus, useRejectWaiting, useSettledProbe, useWaitingServers } from "./use-waiting";
import { WaitingDetailSheet } from "./waiting-detail-sheet";
import {
  disksText,
  hostnameOrIpMatcher,
  providerOf,
  READINESS_FILTERS,
  readiness,
  type ReadinessFilter,
  selectable,
  sizeText,
  statusCell,
  volumesText,
} from "./waiting-row";

export const ROW_PX = 52;
export const VISIBLE_ROWS = 10;
const BOX_RECT = { width: 0, height: ROW_PX * VISIBLE_ROWS };
const GRID = "grid grid-cols-[20px_minmax(0,1fr)_130px_92px_150px_150px_160px] items-center gap-3 px-3";

export function WaitingSection({ organizationSlug, token }: { organizationSlug: string; token: string | undefined }) {
  const waiting = useWaitingServers(organizationSlug);
  const enrollments = useEnrollmentStatus(organizationSlug);
  const cancel = useCancelEnrollment(organizationSlug);
  const servers = waiting.data ?? [];
  const attempts = enrollments.data?.attempts ?? [];
  if (waiting.isError || enrollments.isError) return <p role="alert">Couldn’t load waiting servers or enrollments. <Button variant="outline" onClick={() => { void waiting.refetch(); void enrollments.refetch(); }}>Retry</Button></p>;
  return <>
    <EnrollmentCounters waiting={servers} attempts={attempts} joining={enrollments.data?.joining ?? 0} />
    <WaitingList organizationSlug={organizationSlug} servers={servers} now={waiting.dataUpdatedAt} token={token} attempts={attempts} hasFirst={enrollments.data?.hasFirst ?? true} />
    <EnrollingList attempts={attempts} joining={enrollments.data?.joining ?? 0} onCancel={cancel.mutate} />
  </>;
}

export function WaitingList({ organizationSlug, servers, now, token, attempts = [], hasFirst = true }: {
  organizationSlug: string;
  servers: readonly WaitingServerView[];
  now: number;
  token?: string | undefined;
  attempts?: readonly ServerEnrollmentView[];
  hasFirst?: boolean;
}) {
  const [filter, setFilter] = useState<ReadinessFilter>("all");
  const [provider, setProvider] = useState("");
  const [tokenFilter, setTokenFilter] = useState(token ?? "");
  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [openId, setOpenId] = useState<string | null>(null);
  const [enroll, setEnroll] = useState<readonly WaitingServerView[] | null>(null);
  const autoOpened = useRef<string | null>(null);
  const only = servers.length === 1 ? servers[0] : undefined;
  const lone = !hasFirst && only && selectable(only) ? only : undefined;
  useEffect(() => {
    if (lone && autoOpened.current !== lone.id) {
      autoOpened.current = lone.id;
      setEnroll([lone]);
    }
  }, [lone]);
  const { rows: ordered, returned } = waitingAfterReturns(servers, attempts);
  const reject = useRejectWaiting(organizationSlug);

  const counts = new Map<ReadinessFilter, number>([["all", servers.length]]);
  for (const server of servers) counts.set(readiness(server), (counts.get(readiness(server)) ?? 0) + 1);
  const providers = [...new Set(servers.map(providerOf))].sort();
  const tokens = [...new Set(servers.map((server) => server.fleetToken))].sort();
  const shownToken = tokens.includes(tokenFilter) ? tokenFilter : "";
  const matches = hostnameOrIpMatcher(query);
  const rows = ordered.filter((server) =>
    (filter === "all" || readiness(server) === filter)
    && (provider === "" || providerOf(server) === provider)
    && (shownToken === "" || server.fleetToken === shownToken)
    && matches(server));
  const chosen = rows.filter((server) => picked.has(server.id) && selectable(server)).map((server) => server.id);
  const pickedIds = new Set(chosen);
  const full = chosen.length >= SELECTION_MAX;
  const pickable = rows.filter(selectable);
  const allPicked = pickable.length > 0 && pickable.slice(0, SELECTION_MAX).every((server) => pickedIds.has(server.id));
  const open = servers.find((server) => server.id === openId) ?? null;
  const scroller = useRef<HTMLDivElement>(null);
  // Only the rows in view render, so a long wait list costs no more to draw than the box holds.
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroller.current,
    getItemKey: (index) => rows[index]?.id ?? index,
    estimateSize: () => ROW_PX,
    // The box is ten rows tall by construction, so its height is known rather than measured.
    initialRect: BOX_RECT,
    observeElementRect: (_, report) => report(BOX_RECT),
    overscan: 2,
  });
  const items = virtual.getVirtualItems();
  const padTop = items[0]?.start ?? 0;
  const padBottom = virtual.getTotalSize() - (items.at(-1)?.end ?? 0);

  useSettledProbe(organizationSlug, openId === null ? chosen : [openId]);

  const toggle = (id: string, on: boolean) => {
    const next = new Set(chosen);
    if (on) next.add(id);
    else next.delete(id);
    setPicked(next);
  };
  const rejectIds = (ids: readonly string[]) => {
    reject.mutate(ids);
    setPicked(new Set());
    setOpenId(null);
  };

  return (
    <section aria-labelledby="waiting-heading" className="flex flex-col gap-2.5">
      <div className="flex flex-wrap items-center gap-2.5">
        <h2 id="waiting-heading" className="text-sm font-semibold">Waiting</h2>
        <span className="text-xs text-muted-foreground tabular-nums">{servers.length}</span>
        <div className="ml-auto flex flex-wrap items-center gap-1">
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
      </div>
      <div className="overflow-x-auto rounded-lg border">
        <div role="table" aria-label="Waiting servers" className="min-w-[860px] text-[13px]">
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
            <span role="columnheader">Source IP</span>
            <span role="columnheader">Size</span>
            <span role="columnheader">Disks</span>
            <span role="columnheader">Volumes</span>
            <span role="columnheader">Status</span>
          </div>
          <div ref={scroller} role="rowgroup" data-slot="waiting-box" className="overflow-y-auto" style={{ maxHeight: ROW_PX * VISIBLE_ROWS }}>
            {rows.length === 0 ? (
              <div role="row" className={cn(GRID, "border-t text-muted-foreground")} style={{ height: ROW_PX }}>
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
                      now={now}
                      returned={returned.get(server.id)}
                      picked={pickedIds.has(server.id)}
                      full={full}
                      onPick={(on) => toggle(server.id, on)}
                      onOpen={() => setOpenId(server.id)}
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
      {enroll ? <EnrollSheet organizationSlug={organizationSlug} servers={enroll} hasFirst={hasFirst} onClose={() => { setEnroll(null); setPicked(new Set()); }} /> : null}
      <WaitingDetailSheet
        server={open}
        now={now}
        onOpenChange={(next) => { if (!next) setOpenId(null); }}
        onReject={(id) => rejectIds([id])}
      />
    </section>
  );
}

function WaitingRow({ server, now, picked, full, onPick, onOpen, returned }: {
  server: WaitingServerView;
  returned: ServerEnrollmentView | undefined;
  now: number;
  picked: boolean;
  full: boolean;
  onPick: (on: boolean) => void;
  onOpen: () => void;
}) {
  const { report } = server;
  const status = returned ? {
    bold: returned.state === "cancelled" ? "Cancelled" : `Failed at ${returned.failure?.stage ?? returned.stage ?? "start"}`,
    detail: returned.failure?.message ?? "Ready to try again",
  } : statusCell(server, now);
  const disks = disksText(report);
  const volumes = volumesText(report);
  const state = readiness(server);
  return (
    <div
      role="row"
      data-state={state}
      aria-selected={picked}
      onClick={onOpen}
      className={cn(GRID, "cursor-pointer border-t hover:bg-muted/50", picked && "bg-muted")}
      style={{ height: ROW_PX }}
    >
      <span role="cell" onClick={(event) => event.stopPropagation()}>
        <Checkbox
          aria-label={`Select ${report.hostname}`}
          checked={picked}
          disabled={!selectable(server) || (full && !picked)}
          onCheckedChange={(on) => onPick(on)}
        />
      </span>
      <TwoLineCell>
        <button type="button" className="truncate text-left font-medium outline-none focus-visible:underline" onClick={(event) => { event.stopPropagation(); onOpen(); }}>
          {report.hostname}
        </button>
        <span>{report.os.id} {report.os.version}</span>
      </TwoLineCell>
      <TwoLineCell>
        <span className="font-mono">{server.observedIp}</span>
        <span>{providerOf(server)}</span>
      </TwoLineCell>
      <span role="cell" className="truncate text-muted-foreground">{sizeText(report)}</span>
      <span role="cell" className="truncate">
        {disks.main}
        {disks.spare === "" ? null : <span className="text-muted-foreground"> {disks.spare}</span>}
      </span>
      <TwoLineCell><span>{volumes.bold}</span><span>{volumes.detail}</span></TwoLineCell>
      <TwoLineCell>
        <span className={cn("font-medium", state === "blocked" && "text-destructive")}>{status.bold}</span>
        <span>{status.detail}</span>
      </TwoLineCell>
    </div>
  );
}

function TwoLineCell({ children }: { children: [ReactNode, ReactNode] }) {
  const [first, second] = children;
  return (
    <div role="cell" className="flex min-w-0 flex-col leading-tight">
      <span className="truncate">{first}</span>
      <span className="truncate text-xs text-muted-foreground">{second}</span>
    </div>
  );
}
