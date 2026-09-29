import { useState } from "react";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, EnvironmentRef, MoveView } from "@ployz/sdk";
import { ArrowDownIcon, ArrowUpIcon, CircleCheckIcon, MoreVerticalIcon, PowerOffIcon } from "lucide-react";
import { getEnvironmentSummariesCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { Button } from "#/components/ui/button";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import { ItemGroup } from "#/components/ui/item";
import { plural } from "#/modules/branches/branch-plan";
import { movePicks, presentMoveRow } from "#/modules/config-store/store-branches";
import { branchQuery, environmentsQuery, saveQuery, updateQuery, useStoreDeployments, useStoreView } from "#/modules/config-store/store-view.queries";
import { volumeLoss, type VolumeLoss } from "#/modules/config-store/store-volumes";
import { StoreRefused, useStoreWriter } from "#/modules/config-store/store-write";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { actionVariant, NewsRow } from "./BranchNews";
import { ChangeRowItem } from "./ChangeRowItem";
import { SaveButton, saveInfo, Sheet, SwitchField, useRowPicks } from "./SaveSheet";

type Params = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/**
 * A Branch's panel over the Config Store: what Save puts in its Parent, what Update brings from it, and while it comes
 * off the Servers, how that goes. Keep and Close wait in ⋮.
 */
export function StoreBranchPanel() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const branch = useStoreView(params.organizationSlug, branchQuery(store));
  return branch.ok ? <BranchPanel params={params} store={store} branch={branch.value} /> : (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}><span className="font-medium">{params.environmentSlug}</span></CanvasInspectorHeader>
      <Empty><EmptyDescription>Only branches have this panel.</EmptyDescription></Empty>
    </div>
  );
}

function BranchPanel({ params, store, branch }: { params: Params; store: EnvironmentRef; branch: BranchView }) {
  const writer = useStoreWriter(params.organizationSlug);
  const save = useStoreView(params.organizationSlug, saveQuery(store));
  const update = useStoreView(params.organizationSlug, updateQuery(store));
  const listing = useStoreView(params.organizationSlug, environmentsQuery(params.projectSlug));
  const me = listing.ok ? listing.value.environments.find((environment) => environment.name === branch.environment.name) : undefined;
  const closing = useStoreBranchClose(params, store, branch);
  const [asking, setAsking] = useState(false);
  const [saving, setSaving] = useState(false);
  const name = branch.environment.name;
  const saveView = save.ok ? save.value : null;
  const removal = me?.removal ?? null;
  const deletable = !branch.kept && !me?.default;

  const news = [
    removal ? <RemovalNews key="removal" lead name={name} status={removal.status} onFinish={() => void closing.close()} /> : null,
    saveView?.rows.length ? (
      <NewsRow key="save" lead={!removal} icon={<ArrowUpIcon />} title={`${plural(saveView.rows.length, "change")} to save`}
        detail={nodeNames(saveView)}
        action={<Button size="sm" variant={actionVariant(!removal)} onClick={() => setSaving(true)}>Save to {branch.parent}</Button>}
        changes={saveView.rows.map((row) => (
          <ChangeRowItem key={row.row} row={presentMoveRow(row)} conflict={row.conflict ? branch.parent : undefined} />
        ))} />
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
            onClick={() => void writer.commit({ command: "move", from: null, into: store, picks: null, version: update.value.version })}>
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
        <DropdownMenuItem variant="destructive" disabled={Boolean(me?.default) || removal !== null}
          title={me?.default ? `${name} is the project's default` : undefined} onClick={() => setAsking(true)}>
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
        </p>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-4">
        <ItemGroup className="gap-2">
          {news.length ? news : <NewsRow lead icon={<CircleCheckIcon className="text-success" />} title={`Up to date with ${branch.parent}`} />}
        </ItemGroup>
        {closing.dialog}
        <ConfirmDialog open={asking} onOpenChange={setAsking} title={`Close ${name}?`} description="Its services and data go with it."
          actionLabel="Close branch" variant="destructive" onConfirm={() => { setAsking(false); return closing.close(); }} />
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

/** A Branch coming off the Servers: how it goes, and once it's off, the rest of closing it. */
function RemovalNews({ lead, name, status, onFinish }: { lead: boolean; name: string; status: string; onFinish: () => void }) {
  if (status === "applied") {
    return <NewsRow lead={lead} icon={<PowerOffIcon />} title="Off the servers" detail={`Finish closing ${name}`}
      action={<Button size="sm" variant={actionVariant(lead)} onClick={onFinish}>Finish closing</Button>} />;
  }
  return status === "queued" || status === "running" || status === "cancelling"
    ? <NewsRow lead={lead} icon={<PowerOffIcon />} title="Coming off the servers" detail="Then it closes" />
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
    setPending(true);
    try {
      // Awaited: Save rewrites the Parent, and the page moves on once it has.
      await writer.commit({
        command: "move", from: store, into: null, version: view.version,
        picks: movePicks(rows.picks.map(({ row, pick }) => ({ key: row.key, ticked: pick.ticked, choice: row.choice, option: pick.option, value: pick.value }))),
      }).isPersisted.promise;
      toast.success(`Saved to ${into}`);
      onClose();
      await onSaved(deletable && deleteAfter);
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

type Acceptance = Pick<VolumeLoss, "accept" | "version">;

/**
 * Closing a Branch over the Config Store. Never deployed, or already off the Servers, it goes at once and the page
 * opens its Parent. Otherwise it comes off the Servers first, as a Deployment that asks before deleting Volume data;
 * once that's done, its panel finishes closing it.
 */
// TODO(#1267): Environment removal moves to the Environments UI; the removal could then finish by itself.
function useStoreBranchClose(params: Params, store: EnvironmentRef, branch: BranchView) {
  const writer = useStoreWriter(params.organizationSlug);
  const scope = useCollectionScope();
  const navigate = useNavigate();
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const deployments = useStoreDeployments(params.organizationSlug, store).data.pages[0]?.deployments ?? [];
  const { machines } = useRuntimeLens(params.organizationSlug);
  const place = useEnvironmentPlace(params.organizationSlug, environmentId);
  const [loss, setLoss] = useState<DeletionCheck<Acceptance> | null>(null);
  const name = branch.environment.name;
  const latest = deployments[0];
  const offServers = latest === undefined || (latest.remove && latest.status === "applied");

  const leave = () => navigate(getDashboardDestination({
    kind: "environment", organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, environmentSlug: branch.parent,
  }, "architecture"));

  async function takeOff({ accept, version }: { accept: readonly string[]; version: string | null }): Promise<DeletionCheck<Acceptance> | null> {
    try {
      await writer.commit({
        command: "admit", id: crypto.randomUUID(), environment: store, services: [], version, remove: true, accept_volume_loss: [...accept],
      }).isPersisted.promise;
      toast(`${name} is coming off the servers`, { description: "Its panel finishes closing it once it's off." });
      return null;
    } catch (error) {
      const refused = error instanceof StoreRefused ? volumeLoss(error) : null;
      return refused ? {
        items: refused.volumes.map((volume): DeletionItem => ({
          kind: "volume", name: volume.name,
          detail: volume.deletes.flatMap((held) => machines.find((machine) => machine.id === held.machine_id)?.name ?? []).join(", ") || undefined,
        })),
        evidence: { accept: refused.accept, version: refused.version },
      } : null;
    }
  }

  async function close() {
    if (!offServers) {
      setLoss(await takeOff({ accept: [], version: null }));
      return;
    }
    try {
      // Awaited: the page leaves the Branch once it's gone.
      await writer.commit({ command: "remove_environment", environment: store }).isPersisted.promise;
      await reconcileCollection(getEnvironmentSummariesCollection(params.organizationSlug, scope));
      toast.success(`${name} closed`);
      await leave();
    } catch {
      // The writer toasted the refusal.
    }
  }

  return {
    close,
    leave: async () => { await leave(); },
    dialog: (
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => { if (!open) setLoss(null); }}
        title="Closing deletes data"
        place={place}
        confirmLabel="Close branch"
        items={loss?.items}
        callbacks={{
          load: () => Promise.resolve(loss ?? { items: [], evidence: { accept: [], version: "" } }),
          confirm: async (evidence) => (await takeOff(evidence)) ?? undefined,
        }}
      />
    ),
  };
}
