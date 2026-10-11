import { Suspense, useState } from "react";
import { useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, EnvironmentListing, EnvironmentRef, Synced } from "@ployz/sdk";
import { ChevronDownIcon, GitBranchIcon } from "lucide-react";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Button } from "#/components/ui/button";
import { ButtonGroup } from "#/components/ui/button-group";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem,
  DropdownMenuSeparator, DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { plural } from "#/lib/plural";
import { closesIn, syncButtonState } from "#/modules/config-store/store-sync";
import {
  branchQuery, environmentsQuery, fetchStoreView, requireView, servicesQuery, syncQuery,
  useCachedStoreView, useStoreViews, volumesQuery,
} from "#/modules/config-store/store-view.queries";
import { storeEnvironmentTree } from "#/modules/config-store/store-workspace";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { ENVIRONMENT_ROUTE_FROM, type EnvironmentRouteParams } from "../environment-route-paths";
import { useLeaveWhenClosed, useStoreBranchClose } from "./branch-close";
import { SyncDialog } from "./SyncDialog";

/**
 * Sync, at the canvas's top right: it opens the one Sync dialog, which makes a picked place match this one. On a
 * Branch it says what a Sync into its Parent carries and picks the Parent first, and ▾ holds Keep, Shut down and Close.
 */
export function SyncButton() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [branch, listing] = useStoreViews(params.organizationSlug, [branchQuery(store), environmentsQuery(params.projectSlug)] as const);
  useLeaveWhenClosed(params, branch);
  const environments = requireView(listing).environments;
  if (!branch.ok) return <RootSync params={params} store={store} environments={environments} />;
  return <BranchSync params={params} store={store} branch={branch.value} environments={environments} />;
}

/** Every other Environment of the Project, in tree order: where a Sync from `name` can go. */
function placesFrom(environments: readonly EnvironmentListing[], name: string) {
  return storeEnvironmentTree(environments).map(({ environment }) => environment.name).filter((other) => other !== name);
}

/** Lands on where the Sync went, and Undo there takes its changes back out. */
function useSynced(params: EnvironmentRouteParams, from: string) {
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  return (to: string, changes: number, sync: Synced) => {
    const action = { label: "Undo", onClick: () => void writer.commit({ command: "undo_sync", sync: sync.sync }) };
    void navigate(getDashboardDestination({ kind: "environment", ...params, environmentSlug: to }, "architecture"));
    toast.success(`Synced ${plural(changes, "change")} from ${from}`, { action });
  };
}

/** An Environment that isn't a Branch: Sync alone, with no ▾, picking the first other place. */
function RootSync({ params, store, environments }: {
  params: EnvironmentRouteParams; store: EnvironmentRef; environments: readonly EnvironmentListing[];
}) {
  const [into, setInto] = useState<string | null>(null);
  const name = store.environment ?? "";
  const places = placesFrom(environments, name);
  const synced = useSynced(params, name);
  const [first] = places;
  if (first === undefined) return null;
  return (
    <>
      <Button variant="outline" className="pointer-events-auto" onClick={() => setInto(first)}>
        <GitBranchIcon data-icon="inline-start" />Sync
      </Button>
      {into === null ? null : (
        <Suspense fallback={null}>
          <SyncDialog organizationSlug={params.organizationSlug} from={store} into={into} places={places} closable={() => false}
            onClose={() => setInto(null)} onSynced={(to, changes, sync) => { setInto(null); synced(to, changes, sync); }} />
        </Suspense>
      )}
    </>
  );
}

function BranchSync({ params, store, branch, environments }: {
  params: EnvironmentRouteParams; store: EnvironmentRef; branch: BranchView; environments: readonly EnvironmentListing[];
}) {
  const writer = useStoreWriter(params.organizationSlug);
  const scope = useCollectionScope();
  const closing = useStoreBranchClose(params, store, branch);
  // Where the open dialog syncs to.
  const [into, setInto] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const name = branch.environment.name;
  const me = environments.find((environment) => environment.name === name);
  const removal = me?.removal ?? null;
  // Its main half syncs into the Parent.
  const state = syncButtonState(branch, removal);
  // Warm the main half's dialog.
  useCachedStoreView(params.organizationSlug, syncQuery(store, state.into));
  // The Store closes a Branch only once its own Branches are gone.
  const children = environments.filter((environment) => environment.parent === name).map((environment) => environment.name);
  const shuttable = branch.pull_request !== null;
  const synced = useSynced(params, name);

  return (
    <>
      <ButtonGroup className="pointer-events-auto">
        <Button variant="outline" aria-label={state.count === null ? state.label : `${state.label} · ${state.count}`}
          onClick={() => setInto(state.into)}>
          <GitBranchIcon data-icon="inline-start" />
          {state.label}
          {state.count === null ? null : <span className="text-muted-foreground tabular-nums">{state.count}</span>}
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="outline" size="icon" aria-label="More sync actions" />}>
            <ChevronDownIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-auto min-w-56">
            <DropdownMenuGroup>
              <DropdownMenuCheckboxItem checked={branch.kept}
                onCheckedChange={(kept) => void writer.commit({ command: "keep_branch", environment: store, kept })}>
                Keep {name}
                {branch.closes_at === null ? null : <span className="ml-auto pl-4 text-muted-foreground">{closesIn(branch.closes_at)}</span>}
              </DropdownMenuCheckboxItem>
              {/* Off until the next push; one shut down comes back with a Deploy before that. */}
              {shuttable && removal?.status === "applied" && !removal.in_flight ? (
                <DropdownMenuItem onClick={() => {
                  writer.commit({ command: "admit", admit: "deploy", id: crypto.randomUUID(), environment: store, services: [], version: null, accept_volume_loss: [] });
                }}>Deploy {name}</DropdownMenuItem>
              ) : shuttable ? (
                <DropdownMenuItem disabled={removal?.in_flight === true} onClick={() => void closing.start(true)}>
                  Shut down until the next push
                </DropdownMenuItem>
              ) : null}
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuItem variant="destructive" disabled={Boolean(me?.default) || removal?.in_flight === true || children.length > 0}
                title={me?.default ? `${name} is the project's default` : children.length ? `Close its branches first: ${children.join(", ")}` : undefined}
                onClick={() => setAsking(true)}>
                Close {name}…
              </DropdownMenuItem>
            </DropdownMenuGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      </ButtonGroup>
      {into === null ? null : (
        <Suspense fallback={null}>
          <SyncDialog organizationSlug={params.organizationSlug} from={store} into={into} places={placesFrom(environments, name)}
            // Closing after syncs only a Branch that isn't kept, and only into its Parent; a PR Environment closes
            // with its pull request.
            closable={(to) => to === branch.parent && !branch.kept && !me?.default && !shuttable}
            onClose={() => setInto(null)} onSynced={(to, changes, sync) => { setInto(null); synced(to, changes, sync); }} />
        </Suspense>
      )}
      {closing.dialog}
      {/* Everything that goes, by name, and the user types where: a closed Branch doesn't come back. */}
      <DeletionDialog open={asking} onOpenChange={setAsking} title={`Close ${name}?`} place={`${params.projectSlug}/${name}`}
        sentence={branch.to_parent ? <>
          <span className="text-destructive">{plural(branch.to_parent, "change")} not synced to {branch.parent}</span> go with it, and:
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
          confirm: async () => { setAsking(false); await closing.start(false); },
        }} />
    </>
  );
}
