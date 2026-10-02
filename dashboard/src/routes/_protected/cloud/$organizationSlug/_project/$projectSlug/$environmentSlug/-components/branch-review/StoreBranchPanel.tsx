import { useState } from "react";
import { Link, useLoaderData, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, DeploymentStatus, DifferRow, EnvironmentRef, EnvironmentsView, MoveRow, MoveView } from "@ployz/sdk";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { ArrowDownIcon, ArrowUpIcon, CircleCheckIcon, EqualNotIcon, MoreVerticalIcon, PowerOffIcon } from "lucide-react";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Button } from "#/components/ui/button";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { ItemGroup } from "#/components/ui/item";
import { plural } from "#/lib/plural";
import { isNodeRow, movePicks, presentMoveRow } from "#/modules/config-store/store-branches";
import { deploymentStatusLabel } from "#/modules/config-store/store-deployments";
import {
  branchQuery, environmentsQuery, fetchStoreView, saveQuery, servicesQuery, updateQuery, useStoreViews, volumesQuery,
} from "#/modules/config-store/store-view.queries";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { actionVariant, NewsRow } from "./BranchNews";
import { ChangeRowItem } from "./ChangeRowItem";
import { SaveButton, saveInfo, Sheet, SwitchField, useRowPicks } from "./SaveSheet";
import { StorePullRequestNews, useStorePullRequest } from "./StorePullRequestNews";
import { useLeaveWhenClosed, useStoreBranchClose } from "../sync/branch-close";

type Params = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/**
 * A Branch's panel over the Config Store: what Save puts in its Parent, what Update brings from it, and while it comes
 * off the Servers, how that goes. Keep and Close wait in ⋮.
 */
export function StoreBranchPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  // Read together; off a Branch the Store refuses all but the listing.
  const [branch, save, update, listing] = useStoreViews(params.organizationSlug,
    [branchQuery(store), saveQuery(store), updateQuery(store), environmentsQuery(params.projectSlug)] as const);
  useLeaveWhenClosed(params, branch);
  if (branch.ok) return <BranchPanel params={params} store={store} branch={branch.value} save={save} update={update} listing={listing} />;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}><span className="font-medium">{params.environmentSlug}</span></CanvasInspectorHeader>
      {/* The Store says "not a Branch" as invalid_argument; anything else is a failure to show as it is. */}
      <Empty><EmptyDescription>{branch.refusal.code === "invalid_argument" ? "Only branches have this panel." : branch.refusal.message}</EmptyDescription></Empty>
    </div>
  );
}

function BranchPanel({ params, store, branch, save, update, listing }: {
  params: Params; store: EnvironmentRef; branch: BranchView;
  save: StoreResult<MoveView>; update: StoreResult<MoveView>; listing: StoreResult<EnvironmentsView>;
}) {
  const writer = useStoreWriter(params.organizationSlug);
  const me = listing.ok ? listing.value.environments.find((environment) => environment.name === branch.environment.name) : undefined;
  const closing = useStoreBranchClose(params, store, branch);
  const [asking, setAsking] = useState(false);
  const [saving, setSaving] = useState(false);
  const name = branch.environment.name;
  const saveView = save.ok ? save.value : null;
  const removal = me?.removal ?? null;
  const deletable = !branch.kept && !me?.default;
  // The Store closes a Branch only once its own Branches are gone.
  const children = listing.ok ? listing.value.environments.filter((environment) => environment.parent === name).map((environment) => environment.name) : [];
  const scope = useCollectionScope();
  const pr = useStorePullRequest(branch.pull_request);

  const news = [
    removal ? <RemovalNews key="removal" lead name={name} status={removal.status} inFlight={removal.in_flight} label={deploymentStatusLabel(removal)} forgotten={removal.outcome?.type === "forgotten"} shutDown={branch.pull_request !== null}
      onFinish={() => void closing.close()} onStart={() => {
        // Back on the Servers until shut down again; the writer toasts a refusal.
        writer.commit({ command: "admit", admit: "deploy", id: crypto.randomUUID(), environment: store, services: [], version: null, accept_volume_loss: [] });
      }} /> : null,
    // A PR Environment saves into each Destination for the merge, not into its Parent now.
    branch.pull_request ? pr && <StorePullRequestNews key="pr" store={store} view={pr} lead={!removal} onShutDown={() => void closing.shutDown()} /> : saveView?.rows.length ? (
      <NewsRow key="save" lead={!removal} icon={<ArrowUpIcon />} title={`${plural(saveView.rows.length, "change")} to save`}
        detail={nodeNames(saveView.rows)}
        action={<Button size="sm" variant={actionVariant(!removal)} onClick={() => setSaving(true)}>Save to {branch.parent}</Button>}
        changes={saveView.rows.map((row) => (
          <ChangeRowItem key={row.row} row={presentMoveRow(row)} conflict={row.conflict ? branch.parent : undefined} />
        ))} />
    ) : !save.ok ? (
      // What it would save couldn't be read: say why, never "Up to date".
      <NewsRow key="save" lead={!removal} icon={<ArrowUpIcon />} title={`Save to ${branch.parent}`} detail={save.refusal.message} />
    ) : saveView?.differ.length ? (
      // Nothing moves, but it isn't the same: sizing, domains and the Git branch stay each Environment's own.
      <NewsRow key="differ" lead={!removal} icon={<EqualNotIcon />} title={`${plural(saveView.differ.length, "setting")} kept apart from ${branch.parent}`}
        detail={`${nodeNames(saveView.differ.map(differAsRow))} · Save doesn't carry these`}
        changes={saveView.differ.map((row) => <ChangeRowItem key={row.row} row={presentMoveRow(differAsRow(row))} />)} />
    ) : null,
    branch.update.length ? (
      <NewsRow key="update" lead={!removal && !saveView?.rows.length} icon={<ArrowDownIcon />}
        title={`${plural(branch.update.length, "update")} from ${branch.parent}`}
        detail={update.ok ? <>
          {nodeNames(update.value.rows)}
          {conflicts(update.value) ? <span className="text-warning"> · {conflicts(update.value)} changed in {name} too</span> : null}
        </> : update.refusal.message}
        action={update.ok ? (
          <Button size="sm" variant={actionVariant(!removal && !saveView?.rows.length)}
            onClick={() => void writer.commit({ command: "move", move: "update", into: store, picks: null, version: update.value.version })}>
            Update
          </Button>
        ) : null}
        changes={update.ok ? update.value.rows.map((row) => (
          <ChangeRowItem key={row.row} row={presentMoveRow(row)} conflict={row.conflict ? name : undefined} />
        )) : []} />
    ) : null,
  ].filter((item) => item !== null);

  const menu = (
    <DropdownMenu>
      <DropdownMenuTrigger render={<Button variant="ghost" size="icon" aria-label="Branch actions" title="Branch actions" />}>
        <MoreVerticalIcon />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-auto">
        <DropdownMenuCheckboxItem checked={branch.kept}
          onCheckedChange={(kept) => void writer.commit({ command: "keep_branch", environment: store, kept })}>
          Keep this branch
        </DropdownMenuCheckboxItem>
        {branch.pull_request ? (
          <DropdownMenuItem disabled={removal !== null} onClick={() => void closing.shutDown()}>
            Shut down until the next push
          </DropdownMenuItem>
        ) : null}
        <DropdownMenuItem variant="destructive" disabled={Boolean(me?.default) || removal !== null || children.length > 0}
          title={me?.default ? `${name} is the project's default` : children.length ? `Close its branches first: ${children.join(", ")}` : undefined}
          onClick={() => setAsking(true)}>
          Close {name}…
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params} actions={menu}>
        <span className="font-medium">{name}</span>
        <p className="truncate text-sm text-muted-foreground">
          Branch of <Link to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: branch.parent }}
            className="underline underline-offset-4">{branch.parent}</Link>
          {pr?.pull_request ? ` · PR #${pr.pull_request.number} · ${pr.pull_request.title}` : null}
        </p>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
        <ItemGroup className="gap-2">
          {news.length ? news : <NewsRow lead icon={<CircleCheckIcon className="text-success" />} title={`Up to date with ${branch.parent}`} />}
        </ItemGroup>
        {closing.dialog}
        {/* Everything that goes, by name, and the user types where: a closed Branch doesn't come back. */}
        <DeletionDialog open={asking} onOpenChange={setAsking} title={`Close ${name}?`} place={`${params.projectSlug}/${name}`}
          sentence={saveView?.rows.length && !branch.pull_request ? <>
            <span className="text-destructive">{plural(saveView.rows.length, "change")} not saved to {branch.parent}</span> go with it, and:
          </> : undefined}
          confirmLabel="Close branch" callbacks={{
            load: async () => {
              const [services, volumes] = await Promise.all([fetchStoreView(params.organizationSlug, scope, servicesQuery(store)),
                fetchStoreView(params.organizationSlug, scope, volumesQuery(store))]);
              const items: DeletionItem[] = [
                ...services.services.map((service): DeletionItem => ({ kind: "service", name: service.name })),
                ...volumes.volumes.map((volume): DeletionItem => ({ kind: "volume", name: volume.name })),
              ];
              return { items, evidence: null };
            },
            confirm: async () => { setAsking(false); await closing.close(); },
          }} />
        {saving && saveView ? (
          <StoreSaveSheet store={store} branch={branch} view={saveView} deletable={deletable}
            onSaved={(deleteAfter) => deleteAfter ? closing.close() : closing.leave()} onClose={() => setSaving(false)} />
        ) : null}
      </div>
    </div>
  );
}

/** A setting that stays different, worded like a row that moves. */
const differAsRow = ({ row, from, into }: DifferRow): MoveRow => ({ row, conflict: false, from, into });

const conflicts = (view: MoveView) => view.rows.filter((row) => row.conflict).length;

/** "web, api": the nodes the rows touch, once each. */
const nodeNames = (rows: readonly MoveRow[]) => [...new Set(rows.map((row) => presentMoveRow(row).node))].join(", ");

/**
 * A Branch coming off the Servers: how it goes, and once it's off, the rest of closing it. A PR Environment shut down
 * stays off until the pull request's next push brings it back.
 */
function RemovalNews({ lead, name, status, inFlight, label, forgotten, shutDown, onFinish, onStart }: {
  lead: boolean; name: string; status: DeploymentStatus; inFlight: boolean; label: string; forgotten: boolean; shutDown: boolean; onFinish: () => void; onStart: () => void;
}) {
  if (status === "applied" && shutDown) {
    return <NewsRow lead={lead} icon={<PowerOffIcon />} title={forgotten ? label : "Shut down"} detail="The next push brings it back"
      action={<span className="flex gap-2">
        <Button size="sm" variant={actionVariant(lead)} onClick={onStart}>Deploy {name}</Button>
        <Button size="sm" variant="outline" onClick={onFinish}>Close</Button>
      </span>} />;
  }
  if (status === "applied") {
    return <NewsRow lead={lead} icon={<PowerOffIcon />} title={label} detail={`Finish closing ${name}`}
      action={<Button size="sm" variant={actionVariant(lead)} onClick={onFinish}>Finish closing</Button>} />;
  }
  return inFlight
    ? <NewsRow lead={lead} icon={<PowerOffIcon />} title={shutDown ? "Shutting down" : "Coming off the servers"}
      detail={shutDown ? "The next push brings it back" : "Then it closes"} />
    : <NewsRow lead={lead} icon={<PowerOffIcon className="text-destructive" />} title="Couldn't come off the servers"
      detail="Some services may still run" action={<Button size="sm" variant={actionVariant(lead)} onClick={onFinish}>Close again</Button>} />;
}

/**
 * Save over the Config Store: what the Branch puts in its Parent, picked row by row, each variable its way. Nothing
 * deploys: the Parent gets them as changes to deploy. A Branch that isn't kept or the default can go after.
 */
function StoreSaveSheet({ store, branch, view, deletable, onSaved, onClose }: {
  store: EnvironmentRef; branch: BranchView; view: MoveView; deletable: boolean;
  onSaved: (deleteAfter: boolean) => Promise<void>; onClose: () => void;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  const rows = useRowPicks(view.rows.map((row) => ({
    row: { key: row.row, node: isNodeRow(row.row), conflict: row.conflict, choice: row.choice ?? undefined },
    presented: presentMoveRow(row),
  })));
  const [deleteAfter, setDeleteAfter] = useState(true);
  const [pending, setPending] = useState(false);
  const into = branch.parent;

  async function save() {
    const saved = writer.commit({
      command: "move", move: "save", from: store, version: view.version,
      picks: movePicks(rows.picks.map(({ row, pick }) => ({ key: row.key, ticked: pick.ticked, choice: row.choice, option: pick.option, value: pick.value }))),
    }).isPersisted.promise;
    if (!(deletable && deleteAfter)) {
      // Saved in the background: the Parent shows the changes to deploy, and a refusal is the writer's toast.
      saved.catch(() => undefined);
      onClose();
      await onSaved(false);
      return;
    }
    setPending(true);
    try {
      // Awaited: closing the Branch after is destructive, so it waits until the Parent has what was saved.
      await saved;
      toast.success(`Saved to ${into}`);
      onClose();
      await onSaved(true);
    } catch {
      // The writer toasted the refusal; a stale review shows the fresh rows.
    } finally {
      setPending(false);
    }
  }

  return (
    <Sheet title={`${plural(rows.picks.length, "change")} for ${into}`} entries={rows.picks} picks={rows} destination={into}
      info={saveInfo(rows, into) ?? `Nothing deploys yet. ${into} gets ${plural(rows.ticked.length, "change")} to deploy.`}
      actions={<SaveButton picks={rows} destination={into} pending={pending} onClick={() => void save()} />} onClose={onClose}>
      {deletable ? <SwitchField id="save-then-delete" label={`Delete ${branch.environment.name} after saving`} checked={deleteAfter} onChange={setDeleteAfter} /> : null}
    </Sheet>
  );
}
