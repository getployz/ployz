import { Suspense, useState } from "react";
import { useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, EnvironmentListing, EnvironmentRef, SyncRow } from "@ployz/sdk";
import { ChevronDownIcon, GitBranchIcon } from "lucide-react";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Button } from "#/components/ui/button";
import { ButtonGroup } from "#/components/ui/button-group";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { plural } from "#/lib/plural";
import { closesIn, syncButtonState, undoPaths } from "#/modules/config-store/store-sync";
import { branchQuery, environmentsQuery, fetchStoreView, servicesQuery, useStoreViews, volumesQuery } from "#/modules/config-store/store-view.queries";
import { storeEnvironmentTree } from "#/modules/config-store/store-workspace";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { useLeaveWhenClosed, useStoreBranchClose } from "./branch-close";
import { SyncDialog } from "./SyncDialog";

type Params = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/**
 * A Branch's one control, at the canvas's top right: it says what a Sync into its Parent carries and opens the Sync
 * dialog; ▾ syncs anywhere else in the Project and holds Keep, Shut down and Close. Elsewhere it isn't there.
 */
export function SyncButton() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [branch, listing] = useStoreViews(params.organizationSlug, [branchQuery(store), environmentsQuery(params.projectSlug)] as const);
  useLeaveWhenClosed(params, branch);
  if (!branch.ok) return null;
  return <BranchSync params={params} store={store} branch={branch.value} environments={listing.ok ? listing.value.environments : []} />;
}

function BranchSync({ params, store, branch, environments }: {
  params: Params; store: EnvironmentRef; branch: BranchView; environments: readonly EnvironmentListing[];
}) {
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  const scope = useCollectionScope();
  const closing = useStoreBranchClose(params, store, branch);
  // Where the open dialog syncs to.
  const [into, setInto] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const name = branch.environment.name;
  const me = environments.find((environment) => environment.name === name);
  const removal = me?.removal ?? null;
  const state = syncButtonState(branch, removal);
  // The Store closes a Branch only once its own Branches are gone.
  const children = environments.filter((environment) => environment.parent === name).map((environment) => environment.name);
  const others = storeEnvironmentTree(environments).map(({ environment }) => environment.name)
    .filter((other) => other !== name && other !== branch.parent);
  const shuttable = branch.pull_request !== null;

  function synced(to: string, rows: readonly SyncRow[]) {
    const receiver = { project: store.project, environment: to };
    setInto(null);
    void navigate(getDashboardDestination({ kind: "environment", ...params, environmentSlug: to }, "architecture"));
    toast.success(`Synced ${plural(rows.length, "change")} from ${name}`, {
      // The synced changes go from the receiver's changes to deploy, and the next Sync offers them again.
      action: { label: "Undo", onClick: () => {
        for (const path of undoPaths(rows)) writer.commit({ command: "discard", environment: receiver, path, version: null });
      } },
    });
  }

  return (
    <>
      <ButtonGroup className="pointer-events-auto">
        <Button variant="outline" aria-label={state.count === null ? state.label : `${state.label} · ${state.count}`}
          onClick={() => setInto(branch.parent)}>
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
              <DropdownMenuItem onClick={() => setInto(branch.parent)}>
                Sync to {branch.parent}
                {branch.to_parent ? <span className="ml-auto pl-4 text-muted-foreground">{plural(branch.to_parent, "change")}</span> : null}
              </DropdownMenuItem>
              {others.map((other) => <DropdownMenuItem key={other} onClick={() => setInto(other)}>Sync to {other}</DropdownMenuItem>)}
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
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
                <DropdownMenuItem disabled={removal?.in_flight === true} onClick={() => void closing.shutDown()}>
                  Shut down until the next push
                </DropdownMenuItem>
              ) : null}
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
          <SyncDialog organizationSlug={params.organizationSlug} from={store} into={into} parent={branch.parent}
            // Closing after syncs only a Branch that isn't kept, and only into its Parent.
            closable={into === branch.parent && !branch.kept && !me?.default}
            onClose={() => setInto(null)} onSynced={(rows) => synced(into, rows)} />
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
          confirm: async () => { setAsking(false); await closing.close(); },
        }} />
    </>
  );
}
