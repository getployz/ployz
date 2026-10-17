import { Button } from "#/components/ui/button";
import { cn } from "#/lib/utils";
import type { ServerEnrollmentView } from "#/modules/machines/waiting/enroll.server";
import { ENROLLMENT_STAGES, type EnrollmentStage } from "#/modules/machines/waiting/server-enrollment";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { readiness } from "./waiting-row";

const GRID = "grid grid-cols-[12px_minmax(120px,1fr)_130px_120px_180px_64px] items-center gap-3 px-3";
const STAGES = {
  installing: "Installing", storage: "Setting up ZFS", enrolling: "Enrolling", queued: "In line", joining: "Joining", publishing: "Going live",
} satisfies Record<EnrollmentStage, string>;

export function enrollmentStage(row: ServerEnrollmentView) {
  if (row.state === "joined") return "Joined";
  if (row.queued) return "Queued";
  if (row.state === "awaiting_founder") return "Waiting on first";
  if (row.state === "instructed") return "Starting";
  const stage = ENROLLMENT_STAGES.find((stage) => stage === row.stage);
  return stage === undefined ? "Starting" : STAGES[stage];
}

export function waitingAfterReturns(servers: readonly WaitingServerView[], attempts: readonly ServerEnrollmentView[]) {
  const latest = new Map<string, ServerEnrollmentView>();
  for (const row of attempts) latest.set(row.waitingServerId, row);
  const returned = new Map<string, ServerEnrollmentView>();
  for (const [id, row] of latest) if (row.state === "failed" || row.state === "cancelled") returned.set(id, row);
  const rows = [...servers].sort((a, b) => {
    const aEnd = returned.get(a.id)?.endedAt;
    const bEnd = returned.get(b.id)?.endedAt;
    if (aEnd && bEnd) return aEnd.localeCompare(bEnd) || a.firstSeenAt.localeCompare(b.firstSeenAt) || a.id.localeCompare(b.id);
    if (aEnd || bEnd) return aEnd ? 1 : -1;
    return a.firstSeenAt.localeCompare(b.firstSeenAt) || a.id.localeCompare(b.id);
  });
  return { rows, returned };
}

export function EnrollmentCounters({ waiting, attempts }: { waiting: readonly WaitingServerView[]; attempts: readonly ServerEnrollmentView[] }) {
  const active = attempts.filter((row) => row.endedAt === null);
  const latest = new Map(attempts.map((row) => [row.waitingServerId, row]));
  return <Counters label="Server totals" values={[
    ["Waiting", String(waiting.length)],
    ["Ready", String(waiting.filter((row) => readiness(row) === "ready").length)],
    ["Enrolling", String(active.length)],
    ["Joining", `${active.filter((row) => row.stage === "joining" || row.stage === "publishing").length} / 8`],
    ["Joined", String([...latest.values()].filter((row) => row.state === "joined").length)],
    ["Failed", String([...latest.values()].filter((row) => row.state === "failed").length)],
  ]} />;
}

function Counters({ label, values }: { label: string; values: readonly (readonly [string, string])[] }) {
  return <div className="overflow-x-auto"><dl role="group" aria-label={label} className="grid min-w-[780px] grid-cols-6 divide-x rounded-lg border">
    {values.map(([name, value]) => <div key={name} data-counter={name} className="flex min-w-0 items-center justify-between gap-2 px-3 py-2 text-xs">
      <dt className="truncate text-muted-foreground">{name}</dt><dd className="w-14 shrink-0 text-right font-medium tabular-nums">{value}</dd>
    </div>)}
  </dl></div>;
}

export function EnrollingList({ attempts, onCancel }: { attempts: readonly ServerEnrollmentView[]; onCancel: (id: string) => void }) {
  const rows = attempts.filter((row) => row.endedAt === null || row.state === "joined");
  if (rows.length === 0) return null;
  const active = rows.filter((row) => row.endedAt === null);
  const starting = active.filter((row) => row.state === "awaiting_founder" || row.state === "instructed").length;
  return <section aria-labelledby="enrolling-heading" className="flex flex-col gap-2.5">
    <div className="flex items-center gap-2.5"><h2 id="enrolling-heading" className="text-sm font-semibold">Enrolling</h2>
      <span className="text-xs text-muted-foreground tabular-nums">{rows.length - active.length} of {rows.length} joined</span>
    </div>
    <Counters label="Enrollment stages" values={[
      ["Starting", String(starting)],
      ["Installing", String(active.filter((row) => row.state === "running" && row.stage === "installing").length)],
      ["Setting up ZFS", String(active.filter((row) => row.state === "running" && row.stage === "storage").length)],
      ["Enrolling", String(active.filter((row) => row.state === "running" && row.stage === "enrolling").length)],
      ["In line", String(active.filter((row) => row.state === "running" && row.stage === "queued").length)],
      ["Joining", `${active.filter((row) => row.stage === "joining" || row.stage === "publishing").length} / 8`],
    ]} />
    <div className="overflow-x-auto rounded-lg border">
      <div role="table" aria-label="Enrolling servers" className="min-w-[780px] text-sm">
        <div role="row" className={cn(GRID, "h-9 text-xs text-muted-foreground")}>
          {["", "Name", "Stage", "Steps", "Detail", "Action"].map((name, i) => <span role="columnheader" key={i}>{name}</span>)}
        </div>
        <div role="rowgroup" data-slot="enrolling-box" className="overflow-y-auto" style={{ maxHeight: 520 }}>
          {rows.map((row) => {
            const joined = row.state === "joined";
            const stage = enrollmentStage(row);
            const step = ENROLLMENT_STAGES.indexOf(ENROLLMENT_STAGES.find((value) => value === row.stage) ?? "installing");
            return <div key={row.id} role="row" data-enrollment-id={row.id} className={cn(GRID, "border-t")} style={{ height: 52 }}>
              <span role="cell" aria-hidden="true" className={cn("size-2 rounded-full", joined ? "bg-success" : "bg-muted-foreground")} />
              <span role="cell" className="truncate font-medium">{row.name}{row.role === "founder" && !joined ? <span className="text-muted-foreground"> · first</span> : null}</span>
              <span role="cell" className={cn("truncate", joined ? "text-success" : "text-muted-foreground")}>{stage}</span>
              <div role="cell" aria-label={`${row.name} progress`} className="grid grid-cols-6 gap-1">
                {ENROLLMENT_STAGES.map((value, index) => <span key={value} data-segment={value} className={cn("h-1 rounded-full", joined || (row.state === "running" && !row.queued && index <= step) ? "bg-success" : "bg-muted")} />)}
              </div>
              <span role="cell" className="truncate text-xs text-muted-foreground" title={row.failure?.message ?? undefined}>
                {row.ahead !== null ? `${row.ahead} servers ahead` : row.stage === "queued" ? "Position unknown" : row.queued ? "Waiting for preparation" : row.storage.kind === "zfs" ? row.storage.device ?? "ZFS on main disk" : "Docker only"}
              </span>
              <span role="cell">{joined ? null : <Button size="sm" variant="ghost" aria-label={`Cancel ${row.name}`} onClick={() => onCancel(row.id)}>Cancel</Button>}</span>
            </div>;
          })}
        </div>
      </div>
    </div>
  </section>;
}
