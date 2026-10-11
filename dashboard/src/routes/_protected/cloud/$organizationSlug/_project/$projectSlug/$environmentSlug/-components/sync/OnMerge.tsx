import { useState } from "react";
import { Schema } from "effect";
import { useLoaderData, useParams, useRouterState } from "@tanstack/react-router";
import type { EnvironmentRef, MergeRow, OnMergeCheck, PullRequestRef } from "@ployz/sdk";
import { CircleCheckIcon, CircleMinusIcon, EllipsisIcon, ExternalLinkIcon, GitMergeIcon, LockIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Input } from "#/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "#/components/ui/popover";
import { cn } from "#/lib/utils";
import { settingName } from "#/modules/config-store/store-branches";
import { onMergeCount, onMergeLists, onMergeQuery, prPlansQuery, pullRequestQuery } from "#/modules/config-store/store-pull-requests";
import { branchQuery, useStoreView, useStoreViews } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * A PR Environment's On merge button, while its pull request is open: how many changes the merge lands, and the list
 * of them. The pull request's check links here with `?panel=on-merge`, which opens it.
 */
export function OnMergeButton() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const branch = useStoreView(params.organizationSlug, branchQuery(store));
  const pullRequest = branch.ok ? branch.value.pull_request : null;
  if (pullRequest === null) return null;
  return <OnMergeOpen organizationSlug={params.organizationSlug} project={params.projectSlug} store={store} pullRequest={pullRequest} />;
}

function OnMergeOpen({ organizationSlug, project, store, pullRequest }: {
  organizationSlug: string; project: string; store: EnvironmentRef; pullRequest: PullRequestRef;
}) {
  const [list, facts, plans] = useStoreViews(organizationSlug, [
    onMergeQuery(store), pullRequestQuery(pullRequest), prPlansQuery(project),
  ] as const);
  const linked = useRouterState({ select: (state) => new URLSearchParams(state.location.searchStr).get("panel") === "on-merge" });
  const [open, setOpen] = useState(linked);
  if (!list.ok || !facts.ok || facts.value.pull_request?.open !== true) return null;
  const count = onMergeCount(list.value);
  const repository = plans.ok ? plans.value.plans.find((plan) => plan.repository_id === pullRequest.repository_id)?.repository : undefined;
  const lists = onMergeLists(list.value);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger render={<Button variant="outline" className="pointer-events-auto" aria-label={`On merge · ${count}`} />}>
        <GitMergeIcon data-icon="inline-start" />On merge<span className="text-muted-foreground tabular-nums">· {count}</span>
      </PopoverTrigger>
      <PopoverContent align="end" padding="none" className="w-[34rem]" aria-label="On merge">
        <div className="flex items-baseline gap-2 border-b px-4 py-3">
          <span className="font-medium">On merge</span>
          <span className="text-muted-foreground">#{pullRequest.number} → {lists.map((into) => into.name).join(", ")}</span>
        </div>
        {lists.every((into) => into.rows.length === 0) ? <p className="px-4 py-3 text-muted-foreground">No changes.</p> : lists.map((into) => (
          <ul key={into.name} aria-label={`Into ${into.name}`} className="max-h-96 overflow-y-auto">
            {lists.length > 1 ? <li className="px-4 pt-2 text-xs text-muted-foreground">{into.name}</li> : null}
            {into.rows.map((row) => (
              <OnMergeRow key={row.row} row={row} organizationSlug={organizationSlug} store={store} into={into.name} />
            ))}
          </ul>
        ))}
        <CheckFooter check={facts.value.check}
          href={repository ? `https://github.com/${repository}/pull/${pullRequest.number}/checks` : null} />
      </PopoverContent>
    </Popover>
  );
}

/** What the pull request's GitHub check says, as GitHub shows it. */
function CheckFooter({ check, href }: { check: OnMergeCheck | null; href: string | null }) {
  return (
    <div className="flex items-center gap-2 border-t px-4 py-2.5 text-sm">
      {check === null ? null : <>
        {check.conclusion === "success"
          ? <CircleCheckIcon className="size-4 text-success" aria-label="Passing" />
          : <CircleMinusIcon className="size-4 text-muted-foreground" aria-label="Neutral" />}
        <span className="truncate">{check.title}</span>
      </>}
      {href === null ? null : (
        <a href={href} target="_blank" rel="noreferrer" className="ml-auto flex items-center gap-1 text-muted-foreground hover:text-foreground">
          GitHub<ExternalLinkIcon className="size-3" />
        </a>
      )}
    </div>
  );
}

const isSecret = Schema.is(Schema.Struct({ secret: Schema.Boolean }));
/** A secret the Destination has no value for: the merge lands it Not set. */
const notSet = Schema.is(Schema.Struct({ secret: Schema.Literal(false) }));
const isText = Schema.is(Schema.String);
/** A value as the list shows it; a secret's never comes back. */
const text = (value: MergeRow["lands"]) => isText(value) ? value : JSON.stringify(value);

/**
 * One row: its Service, its key, and what the merge does there. Its ⋯ edits what it lands (a variable), resets an Edit,
 * and excludes it (it stays in the PR Environment) or includes it again.
 */
function OnMergeRow({ row, organizationSlug, store, into }: {
  row: MergeRow; organizationSlug: string; store: EnvironmentRef; into: string;
}) {
  const writer = useStoreWriter(organizationSlug);
  const [editing, setEditing] = useState(false);
  const { name } = row.name === null ? { name: row.node } : settingName(row.name);
  const excluded = row.ride === "excluded";
  const edited = row.ride === "edited";
  const preview = store.environment ?? "";
  const exclude = (off: boolean) => void writer.commit({ command: "never_sync", environment: store, rows: [row.row], off });
  const land = (value?: string) =>
    void writer.commit({ command: "set_merge_value", environment: store, into: { project: store.project, environment: into }, row: row.row, value });
  return (
    <li className={cn("grid grid-cols-[6rem_minmax(0,9rem)_minmax(0,1fr)_auto] items-center gap-x-3 border-b px-4 py-2 last:border-b-0",
      excluded && "text-muted-foreground")}>
      <span className="truncate font-mono text-xs text-muted-foreground">{row.node}</span>
      <span className={cn("truncate font-mono text-xs", excluded && "opacity-60")}>{name}</span>
      <span className={cn("flex min-w-0 flex-wrap items-center gap-1.5 font-mono text-xs", excluded && "opacity-60")}>
        <RowValue row={row} preview={preview} />
        {excluded ? <Badge variant="secondary">Excluded</Badge> : null}
      </span>
      <span className="flex items-center gap-1">
        {excluded ? <Button variant="link" size="sm" className="text-muted-foreground" onClick={() => exclude(true)}>Include</Button> : null}
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={`More for ${name}`} />}>
            <EllipsisIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {!excluded && row.name?.startsWith("env.") ? <DropdownMenuItem onClick={() => setEditing(true)}>Edit</DropdownMenuItem> : null}
            {edited ? <DropdownMenuItem onClick={() => land()}>Reset</DropdownMenuItem> : null}
            <DropdownMenuItem onClick={() => exclude(excluded)}>{excluded ? "Include" : "Exclude"}</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </span>
      {editing ? <EditValue row={row} name={name} into={into} preview={preview} onClose={() => setEditing(false)}
        onSave={(value) => { setEditing(false); land(value); }} /> : null}
    </li>
  );
}

/** What the merge does to the row: production's value → what lands, with whatever the user should know. */
function RowValue({ row, preview }: { row: MergeRow; preview: string }) {
  const edited = row.ride === "edited";
  if (isSecret(row.preview) || isSecret(row.production)) {
    return <>
      <Badge variant="secondary"><LockIcon />Sealed</Badge>
      {notSet(row.lands)
        ? <Badge variant="warning">Not set in production</Badge>
        : edited ? <>
          <span className="text-muted-foreground">{row.production === null ? "New" : "••••••"}</span>
          <span className="text-muted-foreground">→</span>••••••<Badge variant="secondary">Edited</Badge>
        </> : null}
    </>;
  }
  return <>
    {row.conflict ? <span className="font-sans text-muted-foreground">production</span> : null}
    {row.production === null ? <span className="font-sans text-muted-foreground">New</span> : <span>{text(row.production)}</span>}
    <span className="text-muted-foreground">→</span>
    {row.lands === null && !edited ? <Badge variant="secondary">Removed</Badge> : <span>{text(row.lands)}</span>}
    {edited ? <>
      <Badge variant="secondary">Edited</Badge>
      <span className="basis-full font-sans text-muted-foreground">{preview}: {row.preview === null ? "removed" : text(row.preview)}</span>
    </> : null}
  </>;
}

/**
 * What the merge lands in a variable instead: a secret sealed and never shown back; any other value starts from what
 * lands now, or production's value where the PR Environment removed it.
 */
function EditValue({ row, name, into, preview, onClose, onSave }: {
  row: MergeRow; name: string; into: string; preview: string; onClose: () => void; onSave: (value: string) => void;
}) {
  const secret = isSecret(row.preview) || isSecret(row.production);
  const start = secret ? "" : row.lands === null && row.ride !== "edited" ? row.production : row.lands;
  const [value, setValue] = useState(isText(start) ? start : "");
  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle><span className="font-mono">{name}</span> <span className="font-normal text-muted-foreground">· {into}</span></DialogTitle>
          <DialogDescription className="flex items-center gap-1">
            {secret ? <><LockIcon className="size-3" />Sealed</>
              : <>{preview}: <span className="font-mono">{row.preview === null ? "removed" : text(row.preview)}</span></>}
          </DialogDescription>
        </DialogHeader>
        <form id={`edit-${row.row}`} onSubmit={(event) => { event.preventDefault(); onSave(value); }}>
          <Input aria-label={`${name} in ${into}`} type={secret ? "password" : "text"} autoComplete="off" placeholder={secret ? "Value" : undefined}
            value={value} onChange={(event) => setValue(event.target.value)} className="ph-no-capture font-mono" autoFocus />
        </form>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>Cancel</Button>
          <Button type="submit" form={`edit-${row.row}`}>Save</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
