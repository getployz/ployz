import { useEffect, useState } from "react";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, DeploymentStatus, EnvironmentRef, EnvironmentsView, MoveView } from "@ployz/sdk";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { ArrowDownIcon, ArrowUpIcon, CircleCheckIcon, MoreVerticalIcon, PowerOffIcon } from "lucide-react";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { Button } from "#/components/ui/button";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { ItemGroup } from "#/components/ui/item";
import { plural } from "#/lib/plural";
import { movePicks, presentMoveRow } from "#/modules/config-store/store-branches";
import {
  branchQuery, environmentsQuery, fetchStoreView, saveQuery, servicesQuery, updateQuery, useStoreDeployments, useStoreViews, volumesQuery,
} from "#/modules/config-store/store-view.queries";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useVolumeLossCheck, type VolumeAcceptance as Acceptance } from "#/modules/config-store/use-volume-loss-check";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { actionVariant, NewsRow } from "./BranchNews";
import { ChangeRowItem } from "./ChangeRowItem";
import { SaveButton, saveInfo, Sheet, SwitchField, useRowPicks } from "./SaveSheet";
import { StorePullRequestNews, useStorePullRequest } from "./StorePullRequestNews";

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
    removal ? <RemovalNews key="removal" lead name={name} status={removal.status} inFlight={removal.in_flight} shutDown={branch.pull_request !== null}
      onFinish={() => void closing.close()} onStart={() => {
        // Back on the Servers until shut down again; the writer toasts a refusal.
        writer.commit({ command: "admit", admit: "deploy", id: crypto.randomUUID(), environment: store, services: [], version: null, accept_volume_loss: [] });
      }} /> : null,
    // A PR Environment saves into each Destination for the merge, not into its Parent now.
    branch.pull_request ? pr && <StorePullRequestNews key="pr" store={store} view={pr} lead={!removal} onShutDown={() => void closing.shutDown()} /> : saveView?.rows.length ? (
      <NewsRow key="save" lead={!removal} icon={<ArrowUpIcon />} title={`${plural(saveView.rows.length, "change")} to save`}
        detail={nodeNames(saveView)}
        action={<Button size="sm" variant={actionVariant(!removal)} onClick={() => setSaving(true)}>Save to {branch.parent}</Button>}
        changes={saveView.rows.map((row) => (
          <ChangeRowItem key={row.row} row={presentMoveRow(row)} conflict={row.conflict ? branch.parent : undefined} />
        ))} />
    ) : !save.ok ? (
      // What it would save couldn't be read: say why, never "Up to date".
      <NewsRow key="save" lead={!removal} icon={<ArrowUpIcon />} title={`Save to ${branch.parent}`} detail={save.refusal.message} />
    ) : null,
    branch.update.length ? (
      <NewsRow key="update" lead={!removal && !saveView?.rows.length} icon={<ArrowDownIcon />}
        title={`${plural(branch.update.length, "update")} from ${branch.parent}`}
        detail={update.ok ? <>
          {nodeNames(update.value)}
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

const conflicts = (view: MoveView) => view.rows.filter((row) => row.conflict).length;

/** "web, api": the nodes the rows touch, once each. */
const nodeNames = (view: MoveView) => [...new Set(view.rows.map((row) => presentMoveRow(row).node))].join(", ");

/**
 * A Branch coming off the Servers: how it goes, and once it's off, the rest of closing it. A PR Environment shut down
 * stays off until the pull request's next push brings it back.
 */
function RemovalNews({ lead, name, status, inFlight, shutDown, onFinish, onStart }: {
  lead: boolean; name: string; status: DeploymentStatus; inFlight: boolean; shutDown: boolean; onFinish: () => void; onStart: () => void;
}) {
  if (status === "applied" && shutDown) {
    return <NewsRow lead={lead} icon={<PowerOffIcon />} title="Shut down" detail="The next push brings it back"
      action={<span className="flex gap-2">
        <Button size="sm" variant={actionVariant(lead)} onClick={onStart}>Deploy {name}</Button>
        <Button size="sm" variant="outline" onClick={onFinish}>Close</Button>
      </span>} />;
  }
  if (status === "applied") {
    return <NewsRow lead={lead} icon={<PowerOffIcon />} title="Off the servers" detail={`Finish closing ${name}`}
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
    row: { key: row.row, node: !row.row.includes("."), conflict: row.conflict, choice: row.choice ?? undefined },
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


/**
 * Closing a Branch over the Config Store. Never deployed, or already off the Servers, it goes at once and the page
 * opens its Parent. Otherwise it comes off the Servers first, as a Deployment that asks before deleting Volume data and
 * that closes it once applied: the Store deletes it then, whether or not this panel is still open to finish.
 */
function useStoreBranchClose(params: Params, store: EnvironmentRef, branch: BranchView) {
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  const deployments = useStoreDeployments(params.organizationSlug, store).data.pages[0]?.deployments ?? [];
  const lossOf = useVolumeLossCheck(params.organizationSlug);
  const place = `${store.project ?? ""}/${store.environment ?? ""}`;
  const [loss, setLoss] = useState<DeletionCheck<Acceptance> | null>(null);
  // Shutting down takes it off the Servers and keeps it; closing then removes it.
  const [shutting, setShutting] = useState(false);
  // Closing started taking it off the Servers: once it's off, the panel finishes closing it.
  const [closing, setClosing] = useState(false);
  const name = branch.environment.name;
  const latest = deployments[0];
  const offServers = latest === undefined || (latest.remove && latest.status === "applied");
  useEffect(() => {
    if (closing && offServers) void close();
  }, [closing, offServers]);

  const leave = () => navigate(getDashboardDestination({
    kind: "environment", organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, environmentSlug: branch.parent,
  }, "architecture"));

  async function takeOff({ accept, version }: { accept: readonly string[]; version: string | null }, shut = shutting): Promise<DeletionCheck<Acceptance> | null> {
    try {
      await writer.commit({
        command: "admit", admit: "remove", id: crypto.randomUUID(), environment: store, version, accept_volume_loss: [...accept],
        close: !shut,
      }, ["confirmation_required"]).isPersisted.promise;
      toast(`${name} is coming off the servers`,
        { description: shut ? "The pull request's next push brings it back." : "It closes once it's off." });
      return null;
    } catch (error) {
      return error instanceof StoreRefused ? lossOf(error) : null;
    }
  }

  async function shutDown() {
    setShutting(true);
    setLoss(await takeOff({ accept: [], version: null }, true));
  }

  async function close() {
    setShutting(false);
    if (!offServers) {
      const asked = await takeOff({ accept: [], version: null }, false);
      setLoss(asked);
      setClosing(asked === null);
      return;
    }
    setClosing(false);
    try {
      // Awaited: the page leaves the Branch once it's gone. The Store may have deleted it already: that's closed too.
      await writer.commit({ command: "remove_environment", environment: store }, ["not_found"]).isPersisted.promise
        .catch((error: Error) => { if (!(error instanceof StoreRefused && error.code === "not_found")) throw error; });
      toast.success(`${name} closed`);
      await leave();
    } catch {
      // The writer toasted the refusal.
    }
  }

  return {
    close,
    shutDown,
    leave: async () => { await leave(); },
    dialog: (
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => { if (!open) setLoss(null); }}
        title={shutting ? "Shutting down deletes data" : "Closing deletes data"}
        place={place}
        confirmLabel={shutting ? "Shut down" : "Close branch"}
        items={loss?.items}
        callbacks={{
          load: () => Promise.resolve(loss ?? { items: [], evidence: { accept: [], version: "" } }),
          confirm: async (evidence) => {
            const again = await takeOff(evidence);
            if (again === null && !shutting) setClosing(true);
            return again ?? undefined;
          },
        }}
      />
    ),
  };
}
