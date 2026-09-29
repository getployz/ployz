import { useState, type ReactNode } from "react";
import { useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchChoice, BranchOption } from "@ployz/sdk/config";
import { ExternalLinkIcon, GitBranchIcon, GitPullRequestIcon, InfoIcon, TriangleAlertIcon, Undo2Icon, XIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { useSaveBranch } from "#/modules/branches/branch-commands";
import { listNames, plural } from "#/modules/branches/branch-plan";
import { presentRow, type ChangeRow, type PresentedRow } from "#/modules/branches/branch-review";
import type { BranchReviewView, PullRequest } from "#/modules/branches/use-branch-review";
import { useConditionalSave } from "#/modules/pr-environments/conditional-save-commands";
import { githubAppRepository } from "#/modules/pr-environments/repositories";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { cn } from "#/lib/utils";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

type RowPick = { ticked: boolean; option?: BranchOption; value: string };
/**
 * A change as a sheet shows it, from the Cloud document's rows or the Config Store's: `node` adds a whole node, and a
 * `conflict` row changed on the receiving side too.
 */
export type SheetRow = { key: string; node: boolean; conflict: boolean; choice?: BranchChoice | undefined };
/** One row of a sheet. Without `pick` it's read-only. */
type Entry = { row: SheetRow; presented: PresentedRow; choice?: BranchChoice | undefined; pick?: RowPick };
export type Picks = ReturnType<typeof useRowPicks>;

/** A Cloud document row as a sheet row. */
const sheetRow = (row: ChangeRow): SheetRow => ({
  key: row.key, node: row.key.endsWith(":node"), conflict: row.role === "move" && row.conflict, choice: row.role === "move" ? row.choice : undefined,
});
const sheetEntries = (rows: ChangeRow[], nameOf: (lineage: string) => string) =>
  rows.map((row) => ({ row: sheetRow(row), presented: presentRow(row, nameOf) }));

/**
 * A tick per row and a value choice per variable, starting from core's defaults. `sent` is what the server takes: the
 * ticked rows' keys, options and new values ("" for none). Unticking a new node leaves out its settings too.
 */
export function useRowPicks(rows: Array<{ row: SheetRow; presented: PresentedRow }>) {
  const [edits, setEdits] = useState<Record<string, Partial<RowPick>>>({});
  const picks = rows.map(({ row, presented }) => {
    const pick: RowPick = { ticked: true, option: row.choice?.default, value: "", ...edits[row.key] };
    return { row, choice: row.choice, pick, presented };
  });
  const edit = (key: string, change: Partial<RowPick>) => setEdits((current) => ({ ...current, [key]: { ...current[key], ...change } }));
  const leftOut = new Set(picks.flatMap(({ row, pick, presented }) => row.node && !pick.ticked ? [presented.lineageId] : []));
  const ticked = picks.filter(({ pick, presented }) => pick.ticked && !leftOut.has(presented.lineageId));
  return {
    picks, edit, ticked,
    missing: ticked.find(({ pick }) => pick.option === "new" && !pick.value),
    sent: ticked.map(({ row, pick }) => ({ key: row.key, option: pick.option, value: pick.option === "new" ? pick.value : "" })),
  };
}

/**
 * Save: what a Branch puts in its Parent. Nothing deploys: the Parent gets them as its changes to deploy. Unless the
 * Branch is kept or is the Default Environment, it's deleted after saving by default.
 */
export function SaveSheet({ review, branchId, onClose }: { review: BranchReviewView; branchId: string; onClose: () => void }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const name = useEnvironmentDocument(params.organizationSlug, branchId)?.name ?? params.environmentSlug;
  const destination = review.parent;
  const isDefault = useWorkspace(params.organizationSlug).projects.some((project) => project.defaultEnvironmentId === branchId);
  const save = useSaveBranch({ organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, branchName: name, destination });
  const rows = useRowPicks(sheetEntries(review.save, review.nameOf));
  const [deleteAfter, setDeleteAfter] = useState(true);
  const deletable = !review.kept && !isDefault;
  return (
    <Sheet title={`${plural(rows.picks.length, "change")} for ${destination.name}`} entries={rows.picks} picks={rows} destination={destination.name}
      info={saveInfo(rows, destination.name) ?? `Nothing deploys yet. ${destination.name} gets ${plural(rows.ticked.length, "change")} to deploy.`}
      actions={<SaveButton picks={rows} destination={destination.name} pending={save.isPending}
        onClick={() => save.mutate({ branchEnvironmentId: branchId, review: review.saveReview, thenDelete: deletable && deleteAfter, picks: rows.sent })} />}
      error={save.isError ? save.error.message : null} onClose={onClose}>
      {deletable ? <SwitchField id="save-then-delete" label={`Delete ${name} after saving`} checked={deleteAfter} onChange={setDeleteAfter} /> : null}
    </Sheet>
  );
}

/**
 * Save on a PR Environment, into one of its Destinations: the changes go live with the pull request, and it can shut down
 * after saving (off by default).
 */
export function PrSaveSheet({ review, branchId, landing, pullRequest, onClose }: {
  review: BranchReviewView; branchId: string; landing: BranchReviewView["goesTo"][number]; pullRequest: PullRequest; onClose: () => void;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const document = useEnvironmentDocument(params.organizationSlug, branchId);
  const name = document?.name ?? params.environmentSlug;
  const destination = landing.destination.name;
  const { save } = useConditionalSave({ organizationSlug: params.organizationSlug, prEnvironmentId: branchId, destinationEnvironmentId: landing.destination.id });
  const [shutDownAfter, setShutDownAfter] = useState(false);
  const rows = useRowPicks(sheetEntries(landing.rows, review.nameOf));
  const repository = document?.intent.services.map(({ config }) => githubAppRepository(config))
    .find((candidate) => candidate?.repositoryId === pullRequest.repositoryId)?.repository;
  return (
    <Sheet title={`${plural(rows.picks.length, "change")} for ${destination}`} subtitle={<PullRequestLink pr={pullRequest} repository={repository} />}
      entries={rows.picks} picks={rows} destination={destination}
      info={saveInfo(rows, destination) ?? redeployLine(rows.ticked.map(({ presented }) => presented), [pullRequest.number])}
      actions={<SaveButton picks={rows} destination={destination} pending={save.isPending}
        // Awaited, not a per-call callback: saving turns the row to saved, which closes this sheet.
        // A failed save shows in the sheet through save.isError, so the rejection needs no handler here.
        onClick={() => void save.mutateAsync({ review: landing.review, picks: rows.sent, shutDown: shutDownAfter }).then((saved) => {
          toast.success(`Goes live when PR #${pullRequest.number} merges`, saved.shutDown ? { description: `Shutting down ${name}` } : undefined);
          if (shutDownAfter && !saved.shutDown) toast.warning(`${name} is still running`, { description: "It couldn't shut down. Shut it down from the panel's ⋮." });
          onClose();
        }, () => {})} />}
      error={save.isError ? save.error.message : null} onClose={onClose}>
      <SwitchField id="save-then-shut-down" label={`Shut down ${name} now`} description="Starts again on the next push"
        checked={shutDownAfter} onChange={setShutDownAfter} />
    </Sheet>
  );
}

/** What goes live with pull requests, read-only, as the Save sheet showed it. `actions` sit beside the consequence line. */
export function GoesLiveSheet({ title, saves, nameOf, actions, onClose }: {
  title: string;
  saves: Saves;
  /** A lineage's name, as the save's PR Environment calls it. */
  nameOf: (lineage: string, prEnvironmentId: string | null) => string;
  actions?: ReactNode;
  onClose: () => void;
}) {
  const entries = savedEntries(saves, nameOf);
  return (
    <Sheet title={title} entries={entries} info={redeployLine(entries.map(({ presented }) => presented), [...new Set(saves.map((save) => save.prNumber))])}
      actions={actions} onClose={onClose} />
  );
}

type Saves = ReadonlyArray<Pick<ConditionalSaveRow, "prNumber" | "prEnvironmentId" | "rows">>;

function savedEntries(saves: Saves, nameOf: (lineage: string, prEnvironmentId: string | null) => string) {
  return saves.flatMap((save) => save.rows.map(({ row }): Entry => ({ row: sheetRow(row), presented: presentRow(row, (lineage) => nameOf(lineage, save.prEnvironmentId)) })));
}

/** What goes live with pull requests, read-only and neutral, as a section of another sheet. */
export function GoesLiveChanges({ title, saves, nameOf }: { title: string; saves: Saves; nameOf: (lineage: string, prEnvironmentId: string | null) => string }) {
  return (
    <section aria-label={title} className="flex flex-col gap-3">
      <h3 className="font-medium">{title}</h3>
      <ServiceSections entries={savedEntries(saves, nameOf)} />
    </section>
  );
}

/** The sheet: a service per section, a consequence line and its actions. Without `picks` it's read-only. */
export function Sheet({ title, subtitle, entries, picks, destination = "", info, actions, error = null, onClose, children }: {
  title: string; subtitle?: ReactNode; entries: Entry[]; picks?: Picks; destination?: string; info: string; actions: ReactNode;
  error?: string | null; onClose: () => void; children?: ReactNode;
}) {
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {subtitle}
        </DialogHeader>
        <ServiceSections entries={entries} picks={picks} destination={destination} />
        {children}
        {error ? <FieldError>{error}</FieldError> : null}
        <DialogFooter className="sm:items-center">
          <p className="flex flex-1 items-start gap-2 text-muted-foreground"><InfoIcon className="mt-0.5 size-4 shrink-0" />{info}</p>
          {actions}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** A section per service, in the rows' order. */
function ServiceSections({ entries, picks, destination = "" }: { entries: Entry[]; picks?: Picks; destination?: string }) {
  const groups = new Map<string, Entry[]>();
  for (const entry of entries) groups.set(entry.presented.lineageId, [...(groups.get(entry.presented.lineageId) ?? []), entry]);
  return [...groups].map(([lineage, group]) => <ServiceChanges key={lineage} group={group} picks={picks} destination={destination} />);
}

/** Why Save can't run yet, or null. */
export function saveInfo(rows: Picks, destination: string) {
  return rows.missing ? `Enter ${destination}'s value for ${rows.missing.presented.label}.` : rows.ticked.length === 0 ? "Nothing to save." : null;
}

export function SaveButton({ picks, destination, pending, onClick }: { picks: Picks; destination: string; pending: boolean; onClick: () => void }) {
  return (
    <Button disabled={picks.ticked.length === 0 || picks.missing !== undefined || pending} onClick={onClick}>
      {pending ? <Spinner data-icon="inline-start" /> : <GitBranchIcon data-icon="inline-start" />}Save to {destination}
    </Button>
  );
}

export function SwitchField({ id, label, description, checked, onChange }: {
  id: string; label: string; description?: string; checked: boolean; onChange: (checked: boolean) => void;
}) {
  return (
    <FieldLabel htmlFor={id}>
      <Field orientation="horizontal">
        <FieldContent>{label}{description ? <FieldDescription>{description}</FieldDescription> : null}</FieldContent>
        <Switch id={id} checked={checked} onCheckedChange={onChange} />
      </Field>
    </FieldLabel>
  );
}

/** One service: what happens to it, then its settings as Change · Current · New. A new service's own row is its header's ×. */
function ServiceChanges({ group, picks, destination }: { group: Entry[]; picks: Picks | undefined; destination: string }) {
  const node = group.find(({ row }) => row.node);
  const settings = group.filter((entry) => entry !== node);
  const first = group[0];
  if (!first) return null;
  const leftOut = node?.pick !== undefined && !node.pick.ticked;
  return (
    <section className="flex flex-col gap-2 rounded-lg border p-3" aria-label={first.presented.node}>
      <header className="flex items-center gap-2">
        <span className={cn("flex-1", leftOut && "text-muted-foreground line-through")}>
          <span className="font-medium">{first.presented.node}</span> will be {node ? "added" : "updated"}
        </span>
        {settings.length ? <span className="text-muted-foreground">{plural(settings.length, "setting")}</span> : null}
        {node && picks ? <LeaveOut entry={node} picks={picks} /> : null}
      </header>
      {settings.length && !leftOut ? (
        <div className={cn("grid items-center gap-x-2 gap-y-1", picks
          ? "grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)_auto]"
          : "grid-cols-2 sm:grid-cols-3")}>
          <span className="hidden text-muted-foreground sm:block">Change</span>
          <span className="text-muted-foreground">Current</span>
          <span className={cn("text-muted-foreground", picks && "col-span-2")}>New</span>
          {settings.map((entry) => <SettingRow key={entry.row.key} entry={entry} picks={picks} destination={destination} />)}
        </div>
      ) : null}
    </section>
  );
}

/** In the Save sheet, a setting the Destination also changed since branching carries a marker. */
function SettingRow({ entry, picks, destination }: { entry: Entry; picks: Picks | undefined; destination: string }) {
  const { presented, pick, row } = entry;
  const off = pick !== undefined && !pick.ticked;
  return (
    <>
      <span className={cn("col-span-full flex flex-wrap items-center gap-1.5 pt-2 font-medium sm:col-span-1 sm:pt-0", off && "text-muted-foreground line-through")}>
        {presented.label}
        {picks && row.conflict ? <Badge variant="warning"><TriangleAlertIcon />Changed in {destination} too</Badge> : null}
      </span>
      <Value className={cn(off && "opacity-50")}>{presented.before || "—"}</Value>
      {picks ? <><NewValue entry={entry} picks={picks} destination={destination} /><LeaveOut entry={entry} picks={picks} /></>
        : <Value>{presented.after || "—"}</Value>}
    </>
  );
}

/** The value that goes to the Destination. A variable's opens to give the Destination its own value. */
function NewValue({ entry: { row, choice, pick, presented }, picks, destination }: { entry: Entry; picks: Picks; destination: string }) {
  const [editing, setEditing] = useState(false);
  if (!pick?.ticked) return <Value className="opacity-50">{presented.after || "—"}</Value>;
  if (choice && pick.option === "new") {
    return (
      <Input className="font-mono" type={choice.secret ? "password" : "text"} autoComplete="off" autoFocus={editing}
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

function LeaveOut({ entry: { row, pick, presented }, picks }: { entry: Entry; picks: Picks }) {
  const what = `${presented.node}${presented.label && !row.node ? ` · ${presented.label}` : ""}`;
  const ticked = pick?.ticked ?? true;
  return (
    <Button size="icon-sm" variant="ghost" aria-label={ticked ? `Leave out ${what}` : `Save ${what}`}
      title={ticked ? "Leave out" : "Put back"} onClick={() => picks.edit(row.key, { ticked: !ticked })}>
      {ticked ? <XIcon /> : <Undo2Icon />}
    </Button>
  );
}

/** "PR #142 · Add discount codes ↗", to GitHub, where it merges. */
function PullRequestLink({ pr, repository }: { pr: PullRequest; repository: string | undefined }) {
  const label = <><GitPullRequestIcon className="size-3.5" />PR #{pr.number} · {pr.title}</>;
  return (
    <DialogDescription className="flex items-center gap-1.5">
      {repository ? <a href={`https://github.com/${repository}/pull/${pr.number}`} target="_blank" rel="noreferrer" className="flex items-center gap-1.5 hover:text-foreground">
        {label}<ExternalLinkIcon className="size-3.5" />
      </a> : label}
    </DialogDescription>
  );
}

/** "web and worker redeploy when PR #142 merges". */
function redeployLine(rows: PresentedRow[], prNumbers: number[]) {
  const nodes = [...new Set(rows.map((row) => row.node))];
  return `${listNames(nodes)} redeploy${nodes.length === 1 ? "s" : ""} when ${listNames(prNumbers.map((n) => `PR #${n}`))} merge${prNumbers.length === 1 ? "s" : ""}`;
}
