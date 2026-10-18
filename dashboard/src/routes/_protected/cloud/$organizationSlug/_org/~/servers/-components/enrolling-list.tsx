import { useVirtualizer } from "@tanstack/react-virtual";
import { memo, useRef } from "react";
import { Button } from "#/components/ui/button";
import { cn } from "#/lib/utils";
import type { ServerEnrollmentView } from "#/modules/machines/waiting/enroll.server";
import { ENROLLMENT_STAGES, type EnrollmentStage } from "#/modules/machines/waiting/server-enrollment";

const BOX_RECT = { width: 0, height: 520 };
const GRID = "grid grid-cols-[12px_minmax(120px,1fr)_130px_120px_180px_64px] items-center gap-3 px-3";
export const STAGES = {
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

export const EnrollingList = memo(function EnrollingList({ attempts, onCancel }: { attempts: readonly ServerEnrollmentView[]; onCancel: (id: string) => void }) {
  const rows = attempts.filter((row) => row.endedAt === null || row.state === "joined");
  const scroller = useRef<HTMLDivElement>(null);
  const height = Math.min(rows.length, 10) * 52;
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroller.current,
    getItemKey: (index) => rows[index]?.id ?? index,
    estimateSize: () => 52,
    initialRect: BOX_RECT,
    observeElementRect: (_, report) => report(BOX_RECT),
    overscan: 2,
  });
  const items = virtual.getVirtualItems();
  const padTop = items[0]?.start ?? 0;
  const padBottom = virtual.getTotalSize() - (items.at(-1)?.end ?? 0);
  if (rows.length === 0) return null;
  const active = rows.filter((row) => row.endedAt === null);
  return <section aria-labelledby="enrolling-heading" className="flex flex-col gap-2.5">
    <div className="flex items-center gap-2.5"><h2 id="enrolling-heading" className="text-sm font-semibold">Enrolling</h2>
      <span className="text-xs text-muted-foreground tabular-nums">{rows.length - active.length} of {rows.length} joined</span>
    </div>
    <div className="overflow-x-auto rounded-lg border">
      <div role="table" aria-label="Enrolling servers" aria-rowcount={rows.length + 1} className="min-w-[780px] text-sm">
        <div role="row" className={cn(GRID, "h-9 text-xs text-muted-foreground")}>
          {["", "Name", "Stage", "Steps", "Detail", "Action"].map((name, i) => <span role="columnheader" key={i}>{name}</span>)}
        </div>
        <div ref={scroller} role="rowgroup" data-slot="enrolling-box" className="overflow-y-auto" style={{ height, maxHeight: 520 }}>
          {padTop > 0 ? <div role="presentation" style={{ height: padTop }} /> : null}
          {items.map((item) => {
            const row = rows[item.index];
            if (row === undefined) return null;
            const joined = row.state === "joined";
            const stage = enrollmentStage(row);
            const step = ENROLLMENT_STAGES.indexOf(ENROLLMENT_STAGES.find((value) => value === row.stage) ?? "installing");
            return <div key={row.id} role="row" aria-rowindex={item.index + 2} data-enrollment-id={row.id} className={cn(GRID, "border-t")} style={{ height: 52 }}>
              <span role="cell" aria-hidden="true" className={cn("size-2 rounded-full", joined ? "bg-success" : "bg-muted-foreground")} />
              <span role="cell" className="truncate font-medium">{row.name}{row.role === "founder" && !joined ? <span className="text-muted-foreground"> · first</span> : null}</span>
              <span role="cell" className={cn("truncate", joined ? "text-success" : "text-muted-foreground")}>{stage}</span>
              <div role="cell" aria-label={`${row.name} progress`} className="grid grid-cols-6 gap-1">
                {ENROLLMENT_STAGES.map((value, index) => <span key={value} data-segment={value} className={cn("h-1 rounded-full", joined || (row.state === "running" && !row.queued && index <= step) ? "bg-success" : "bg-muted")} />)}
              </div>
              <span role="cell" className="truncate text-xs text-muted-foreground" title={row.failure?.message ?? undefined}>
                {row.ahead !== null ? `${row.ahead} ${row.ahead === 1 ? "server" : "servers"} ahead` : row.stage === "queued" ? "" : row.queued ? "Preparing" : row.storage.kind === "zfs" ? row.storage.device ?? "ZFS on main disk" : "Docker only"}
              </span>
              <span role="cell">{joined ? null : <Button size="sm" variant="ghost" aria-label={`Cancel ${row.name}`} onClick={() => onCancel(row.id)}>Cancel</Button>}</span>
            </div>;
          })}
          {padBottom > 0 ? <div role="presentation" style={{ height: padBottom }} /> : null}
        </div>
      </div>
    </div>
  </section>;
});
