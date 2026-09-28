import { useState } from "react";
import { useParams } from "@tanstack/react-router";
import { GitBranchIcon, InfoIcon, Undo2Icon, XIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { useSaveBranch } from "#/modules/branches/branch-commands";
import { plural } from "#/modules/branches/branch-plan";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { cn } from "#/lib/utils";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useRowPicks } from "./RowPicks";

type Picks = ReturnType<typeof useRowPicks>;
type Picked = Picks["picks"][number];

/**
 * Save: what a Branch puts in its Parent, service by service, as Change · Current · New. × leaves a change out, and
 * tapping New gives the Parent its own value; a new secret asks for one. Nothing deploys: the Parent gets them as its
 * changes to deploy. Unless the Branch is kept or is the Default Environment, it's deleted after saving by default.
 */
export function SaveSheet({ review, branchId, onClose }: { review: BranchReviewView; branchId: string; onClose: () => void }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, branchId)?.name ?? params.environmentSlug;
  const destination = review.parent.name;
  const isDefault = useWorkspace(params.organizationSlug).projects.some((project) => project.defaultEnvironmentId === branchId);
  const save = useSaveBranch({ organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, branchName: name, destination: review.parent });
  const rows = useRowPicks(review.save, review.nameOf);
  const [deleteAfter, setDeleteAfter] = useState(true);
  const deletable = !review.kept && !isDefault;
  const groups = new Map<string, Picked[]>();
  for (const picked of rows.picks) groups.set(picked.presented.lineageId, [...(groups.get(picked.presented.lineageId) ?? []), picked]);
  const count = rows.ticked.length;

  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent padding="none" className="flex max-h-[85dvh] flex-col overflow-hidden sm:max-w-3xl">
        <div className="shrink-0 border-b px-6 py-4 pr-12">
          <DialogTitle>{plural(review.save.length, "change")} for {destination}</DialogTitle>
        </div>
        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-6 py-4">
          {[...groups.values()].map((group) => <ServiceChanges key={group[0]?.presented.lineageId} group={group} picks={rows} destination={destination} />)}
          {deletable ? (
            <FieldLabel htmlFor="save-then-delete">
              <Field orientation="horizontal">
                <FieldContent>Delete {name} after saving</FieldContent>
                <Switch id="save-then-delete" checked={deleteAfter} onCheckedChange={setDeleteAfter} />
              </Field>
            </FieldLabel>
          ) : null}
        </div>
        <div className="flex shrink-0 flex-col gap-3 border-t px-6 py-4 sm:flex-row sm:items-center">
          <p className="flex flex-1 items-start gap-2 text-sm text-muted-foreground">
            <InfoIcon className="mt-0.5 size-4 shrink-0" />
            {rows.missing ? `Enter ${destination}'s value for ${rows.missing.presented.label}.`
              : count === 0 ? "Nothing to save."
              : `Nothing deploys yet. ${destination} gets ${plural(count, "change")} to deploy.`}
          </p>
          <Button disabled={count === 0 || rows.missing !== undefined || save.isPending}
            onClick={() => save.mutate({ branchEnvironmentId: branchId, review: review.saveReview, thenDelete: deletable && deleteAfter, picks: rows.sent })}>
            {save.isPending ? <Spinner data-icon="inline-start" /> : <GitBranchIcon data-icon="inline-start" />}Save to {destination}
          </Button>
        </div>
        {save.isError ? <FieldError className="px-6 pb-4">{save.error.message}</FieldError> : null}
      </DialogContent>
    </Dialog>
  );
}

/** One service: what happens to it, then its settings. A new service's own row is its header's ×. */
function ServiceChanges({ group, picks, destination }: { group: Picked[]; picks: Picks; destination: string }) {
  const node = group.find(({ row }) => row.key.endsWith(":node"));
  const settings = group.filter((picked) => picked !== node);
  const first = group[0];
  if (!first) return null;
  const leftOut = node !== undefined && !node.pick.ticked;
  return (
    <section className="overflow-hidden rounded-lg border" aria-label={first.presented.node}>
      <header className="flex items-center gap-2 px-3 py-2 text-sm">
        <span className={cn("flex-1", leftOut && "text-muted-foreground line-through")}>
          <span className="font-medium">{first.presented.node}</span> will be {node ? "added" : "updated"}
        </span>
        {settings.length ? <span className="text-xs text-muted-foreground">{plural(settings.length, "setting")}</span> : null}
        {node ? <LeaveOut picked={node} picks={picks} /> : null}
      </header>
      {settings.length && !leftOut ? (
        <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-x-2 gap-y-1 border-t bg-muted/40 px-3 py-2 text-xs sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)_auto]">
          <span className="hidden text-muted-foreground sm:block">Change</span>
          <span className="text-muted-foreground">Current</span>
          <span className="col-span-2 text-muted-foreground">New</span>
          {settings.map((picked) => <SettingRow key={picked.row.key} picked={picked} picks={picks} destination={destination} />)}
        </div>
      ) : null}
    </section>
  );
}

function SettingRow({ picked, picks, destination }: { picked: Picked; picks: Picks; destination: string }) {
  const { presented, pick } = picked;
  const off = !pick.ticked;
  return (
    <>
      <span className={cn("col-span-3 pt-2 font-medium sm:col-span-1 sm:pt-0", off && "text-muted-foreground line-through")}>{presented.label}</span>
      <Value className={cn("bg-background", off && "opacity-50")}>{presented.before || "—"}</Value>
      <NewValue picked={picked} picks={picks} destination={destination} />
      <LeaveOut picked={picked} picks={picks} />
    </>
  );
}

/** The value that goes to the Destination. A variable's opens to give the Destination its own value. */
function NewValue({ picked: { row, choice, pick, presented }, picks, destination }: { picked: Picked; picks: Picks; destination: string }) {
  const [editing, setEditing] = useState(false);
  if (!pick.ticked) return <Value className="opacity-50">{presented.after || "—"}</Value>;
  if (choice && pick.option === "new") {
    return (
      <Input className="h-8 font-mono text-xs" type={choice.secret ? "password" : "text"} autoComplete="off" autoFocus={editing}
        value={pick.value} placeholder="new value" aria-label={`${presented.label} in ${destination}`}
        onChange={(event) => picks.edit(row.key, { value: event.target.value })} />
    );
  }
  const shown = pick.option === "leave_out" ? "—" : presented.after || "—";
  return choice ? (
    <button type="button" className="min-w-0 text-left" title={`Give ${destination} its own value`}
      onClick={() => { setEditing(true); picks.edit(row.key, { option: "new", value: "" }); }}>
      <Value className="border-changed-border bg-changed-soft text-changed-deep">{shown}</Value>
    </button>
  ) : <Value className="border-changed-border bg-changed-soft text-changed-deep">{shown}</Value>;
}

function Value({ className, children }: { className?: string; children: string }) {
  return <span className={cn("block truncate rounded-md border px-2 py-1.5 font-mono", className)} title={children}>{children}</span>;
}

function LeaveOut({ picked: { row, pick, presented }, picks }: { picked: Picked; picks: Picks }) {
  const what = `${presented.node}${presented.label && !row.key.endsWith(":node") ? ` · ${presented.label}` : ""}`;
  return (
    <Button size="icon-sm" variant="ghost" aria-label={pick.ticked ? `Leave out ${what}` : `Save ${what}`}
      title={pick.ticked ? "Leave out" : "Put back"} onClick={() => picks.edit(row.key, { ticked: !pick.ticked })}>
      {pick.ticked ? <XIcon /> : <Undo2Icon />}
    </Button>
  );
}
