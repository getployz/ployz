import { useState, type ReactNode } from "react";
import { useParams } from "@tanstack/react-router";
import { ExternalLinkIcon, GitBranchIcon, GitPullRequestIcon, InfoIcon, Undo2Icon, XIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { useSaveBranch } from "#/modules/branches/branch-commands";
import { listNames, plural } from "#/modules/branches/branch-plan";
import { presentRow, type PresentedRow } from "#/modules/branches/branch-review";
import type { BranchReviewView, PullRequest } from "#/modules/branches/use-branch-review";
import { useConditionalSave, usePrEnvironmentOff } from "#/modules/pr-environments/conditional-save-commands";
import { githubAppRepository } from "#/modules/pr-environments/repositories";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
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
 * On a PR Environment, `landing` is one of its Destinations: the changes go live with the pull request instead, and it
 * can shut down after saving (off by default).
 */
export function SaveSheet({ review, branchId, landing, onClose }: {
  review: BranchReviewView; branchId: string; landing?: BranchReviewView["goesTo"][number]; onClose: () => void;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const document = useEnvironmentDocument(params.organizationSlug, branchId);
  const name = document?.name ?? params.environmentSlug;
  const pr = landing ? review.pullRequest : null;
  const target = landing?.destination ?? review.parent;
  const destination = target.name;
  const isDefault = useWorkspace(params.organizationSlug).projects.some((project) => project.defaultEnvironmentId === branchId);
  const saveBranch = useSaveBranch({ organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, branchName: name, destination: target });
  const saveForPr = useConditionalSave({
    organizationSlug: params.organizationSlug, prEnvironmentId: branchId, destinationEnvironmentId: target.id, prNumber: pr?.number ?? 0,
  }).save;
  const save = pr ? saveForPr : saveBranch;
  const { shutDown } = usePrEnvironmentOff({ organizationSlug: params.organizationSlug, environmentId: branchId, name });
  const [shutDownAfter, setShutDownAfter] = useState(false);
  const rows = useRowPicks(landing?.rows ?? review.save, review.nameOf);
  const [deleteAfter, setDeleteAfter] = useState(true);
  const deletable = !pr && !review.kept && !isDefault;
  const groups = new Map<string, Picked[]>();
  for (const picked of rows.picks) groups.set(picked.presented.lineageId, [...(groups.get(picked.presented.lineageId) ?? []), picked]);
  const count = rows.ticked.length;
  const repository = pr && document?.intent.services.map(({ config }) => githubAppRepository(config))
    .find((candidate) => candidate?.repositoryId === pr.repositoryId)?.repository;

  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent padding="none" className="flex max-h-[85dvh] flex-col overflow-hidden sm:max-w-3xl">
        <div className="shrink-0 border-b px-6 py-4 pr-12">
          <DialogTitle>{plural(rows.picks.length, "change")} for {destination}</DialogTitle>
          {pr ? <PullRequestLink pr={pr} repository={repository ?? undefined} /> : null}
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
          {pr ? (
            <FieldLabel htmlFor="save-then-shut-down">
              <Field orientation="horizontal">
                <FieldContent>
                  Shut down {name} now
                  <FieldDescription>Starts again on the next push</FieldDescription>
                </FieldContent>
                <Switch id="save-then-shut-down" checked={shutDownAfter} onCheckedChange={setShutDownAfter} />
              </Field>
            </FieldLabel>
          ) : null}
        </div>
        <div className="flex shrink-0 flex-col gap-3 border-t px-6 py-4 sm:flex-row sm:items-center">
          <p className="flex flex-1 items-start gap-2 text-sm text-muted-foreground">
            <InfoIcon className="mt-0.5 size-4 shrink-0" />
            {rows.missing ? `Enter ${destination}'s value for ${rows.missing.presented.label}.`
              : count === 0 ? "Nothing to save."
              : pr ? goesLive(rows.ticked.map(({ presented }) => presented), [pr.number])
              : `Nothing deploys yet. ${destination} gets ${plural(count, "change")} to deploy.`}
          </p>
          <Button disabled={count === 0 || rows.missing !== undefined || save.isPending}
            onClick={() => landing
              ? saveForPr.mutate({ review: landing.review, picks: rows.sent }, { onSuccess: () => {
                if (shutDownAfter) shutDown.mutate();
                onClose();
              } })
              : saveBranch.mutate({ branchEnvironmentId: branchId, review: review.saveReview, thenDelete: deletable && deleteAfter, picks: rows.sent })}>
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

/** "PR #142 · Add discount codes ↗", to GitHub, where it merges. */
function PullRequestLink({ pr, repository }: { pr: PullRequest; repository: string | undefined }) {
  const label = <><GitPullRequestIcon className="size-3.5" />PR #{pr.number} · {pr.title}</>;
  return repository ? (
    <a href={`https://github.com/${repository}/pull/${pr.number}`} target="_blank" rel="noreferrer"
      className="mt-1 flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground">
      {label}<ExternalLinkIcon className="size-3.5" />
    </a>
  ) : <p className="mt-1 flex items-center gap-1.5 text-sm text-muted-foreground">{label}</p>;
}

/** "web and worker redeploy when PR #142 merges". */
function goesLive(rows: PresentedRow[], prNumbers: number[]) {
  const nodes = [...new Set(rows.map((row) => row.node))];
  return `${listNames(nodes)} redeploy${nodes.length === 1 ? "s" : ""} when ${listNames(prNumbers.map((n) => `PR #${n}`))} merge${prNumbers.length === 1 ? "s" : ""}`;
}

/**
 * What goes live with pull requests, read-only, as the Save sheet showed it: service by service, Current · New.
 * `actions` sit beside the consequence line.
 */
export function GoesLiveSheet({ title, saves, nameOf, actions, onClose }: {
  title: string;
  saves: ReadonlyArray<Pick<ConditionalSaveRow, "prNumber" | "prEnvironmentId" | "rows">>;
  /** A lineage's name, as the save's PR Environment calls it. */
  nameOf: (lineage: string, prEnvironmentId: string | null) => string;
  actions?: ReactNode;
  onClose: () => void;
}) {
  const rows = saves.flatMap((save) => save.rows.map(({ row }) => presentRow(row, (lineage) => nameOf(lineage, save.prEnvironmentId))));
  const groups = new Map<string, PresentedRow[]>();
  for (const row of rows) groups.set(row.lineageId, [...(groups.get(row.lineageId) ?? []), row]);
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent padding="none" className="flex max-h-[85dvh] flex-col overflow-hidden sm:max-w-3xl">
        <div className="shrink-0 border-b px-6 py-4 pr-12"><DialogTitle>{title}</DialogTitle></div>
        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-6 py-4">
          {[...groups.values()].map((group) => {
            const node = group.find((row) => row.key.endsWith(":node"));
            const settings = group.filter((row) => row !== node);
            const first = group[0];
            return first ? (
              <section key={first.lineageId} className="overflow-hidden rounded-lg border" aria-label={first.node}>
                <header className="flex items-center gap-2 px-3 py-2 text-sm">
                  <span className="flex-1"><span className="font-medium">{first.node}</span> will be {node ? "added" : "updated"}</span>
                  {settings.length ? <span className="text-xs text-muted-foreground">{plural(settings.length, "setting")}</span> : null}
                </header>
                {settings.length ? (
                  <div className="grid grid-cols-2 items-center gap-x-2 gap-y-1 border-t bg-muted/40 px-3 py-2 text-xs sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_minmax(0,1fr)]">
                    <span className="hidden text-muted-foreground sm:block">Change</span>
                    <span className="text-muted-foreground">Current</span>
                    <span className="text-muted-foreground">New</span>
                    {settings.map((row) => (
                      <GoesLiveRow key={row.key} row={row} />
                    ))}
                  </div>
                ) : null}
              </section>
            ) : null;
          })}
        </div>
        <div className="flex shrink-0 flex-col gap-3 border-t px-6 py-4 sm:flex-row sm:items-center">
          <p className="flex flex-1 items-start gap-2 text-sm text-muted-foreground">
            <InfoIcon className="mt-0.5 size-4 shrink-0" />{goesLive(rows, [...new Set(saves.map((save) => save.prNumber))])}
          </p>
          {actions}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function GoesLiveRow({ row }: { row: PresentedRow }) {
  return (
    <>
      <span className="col-span-2 pt-2 font-medium sm:col-span-1 sm:pt-0">{row.label}</span>
      <Value className="bg-background">{row.before || "—"}</Value>
      <Value className="bg-background">{row.after || "—"}</Value>
    </>
  );
}
