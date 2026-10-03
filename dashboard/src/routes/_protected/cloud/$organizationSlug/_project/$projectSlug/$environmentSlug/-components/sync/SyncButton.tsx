import { Suspense, useState } from "react";
import { useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, EnvironmentListing, EnvironmentRef, SyncId, Synced } from "@ployz/sdk";
import {
  ChevronDownIcon, CircleCheckIcon, GitBranchIcon, GitPullRequestIcon, TriangleAlertIcon, Undo2Icon,
} from "lucide-react";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Button } from "#/components/ui/button";
import { ButtonGroup } from "#/components/ui/button-group";
import {
  DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel,
  DropdownMenuSeparator, DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { plural } from "#/lib/plural";
import { pullRequestQuery } from "#/modules/config-store/store-pull-requests";
import { closesIn, goesLive, mergeSync, syncButtonState } from "#/modules/config-store/store-sync";
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
 * A Branch's one control, at the canvas's top right: it says what a Sync into its Parent carries and opens the Sync
 * dialog; ▾ syncs anywhere else in the Project and holds Keep, Shut down and Close. On a PR Environment it syncs into
 * the Destination at the merge, reads "Goes live with #N" once that stands, and ▾ adds the GitHub check and Undo.
 * Elsewhere it isn't there.
 */
export function SyncButton() {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [branch, listing] = useStoreViews(params.organizationSlug, [branchQuery(store), environmentsQuery(params.projectSlug)] as const);
  useLeaveWhenClosed(params, branch);
  if (!branch.ok) return null;
  return <BranchSync params={params} store={store} branch={branch.value} environments={requireView(listing).environments} />;
}

function BranchSync({ params, store, branch, environments }: {
  params: EnvironmentRouteParams; store: EnvironmentRef; branch: BranchView; environments: readonly EnvironmentListing[];
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
  // A PR Environment's pull request: its Destination, its Conditional Sync there and its GitHub check. Chrome.
  const pullRequest = useCachedStoreView(params.organizationSlug, branch.pull_request ? pullRequestQuery(branch.pull_request) : null);
  const check = pullRequest?.ok && pullRequest.value.pull_request?.open ? pullRequest.value : null;
  const merge = pullRequest?.ok ? mergeSync(pullRequest.value, name) : null;
  const standing = merge?.standing ?? null;
  // Its main half syncs into the Parent, or a PR Environment's Destination at the merge.
  const state = syncButtonState(branch, removal, merge);
  // Warm the main half's dialog.
  useCachedStoreView(params.organizationSlug, syncQuery(store, state.into));
  // The Store closes a Branch only once its own Branches are gone.
  const children = environments.filter((environment) => environment.parent === name).map((environment) => environment.name);
  const others = storeEnvironmentTree(environments).map(({ environment }) => environment.name)
    .filter((other) => other !== name && other !== state.into);
  const shuttable = branch.pull_request !== null;
  // Takes the synced changes back out of the receiver, or withdraws a Conditional Sync (its id names it too), and
  // the next Sync offers them again.
  const undo = (sync: SyncId) => writer.commit({ command: "undo_sync", sync });

  function synced(to: string, changes: number, sync: Synced) {
    setInto(null);
    const action = { label: "Undo", onClick: () => void undo(sync.sync) };
    if (sync.when.kind === "at_merge") {
      // Nothing is staged in `to` until the merge: the user stays here, where the button now reads "Goes live".
      toast.success(goesLive(changes, to, sync.when.conditional_sync.pull_request), { action });
      return;
    }
    void navigate(getDashboardDestination({ kind: "environment", ...params, environmentSlug: to }, "architecture"));
    toast.success(`Synced ${plural(changes, "change")} from ${name}`, { action });
  }

  return (
    <>
      <ButtonGroup className="pointer-events-auto">
        <Button variant="outline" aria-label={state.count === null ? state.label : `${state.label} · ${state.count}`}
          onClick={() => setInto(state.into)}>
          {standing ? <GitPullRequestIcon data-icon="inline-start" /> : <GitBranchIcon data-icon="inline-start" />}
          {state.label}
          {state.count === null ? null : <span className="text-muted-foreground tabular-nums">{state.count}</span>}
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="outline" size="icon" aria-label="More sync actions" />}>
            <ChevronDownIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-auto min-w-56">
            <DropdownMenuGroup>
              {/* A standing Conditional Sync already holds these changes for the merge; Undo below changes it. */}
              {standing ? null : (
                <DropdownMenuItem onClick={() => setInto(state.into)}>
                  Sync to {state.into}
                  {state.changes ? <span className="ml-auto pl-4 text-muted-foreground">{plural(state.changes, "change")}</span> : null}
                </DropdownMenuItem>
              )}
              {others.map((other) => <DropdownMenuItem key={other} onClick={() => setInto(other)}>Sync to {other}</DropdownMenuItem>)}
            </DropdownMenuGroup>
            {check || standing ? <>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                {check ? (
                  <DropdownMenuLabel className="flex items-start gap-2">
                    {check.passing ? <CircleCheckIcon className="mt-0.5 size-4 shrink-0 text-success" />
                      : <TriangleAlertIcon className="mt-0.5 size-4 shrink-0 text-warning" />}
                    <span>
                      <span className="block text-foreground">{check.passing ? "Ready to merge on GitHub" : "Not ready to merge on GitHub"}</span>
                      {check.reason}
                    </span>
                  </DropdownMenuLabel>
                ) : null}
                {/* Withdraws the Conditional Sync: the changes no longer go live with the merge. */}
                {merge && standing ? (
                  <DropdownMenuItem onClick={() => void undo(standing)}>
                    <Undo2Icon />Undo sync to {merge.into}
                  </DropdownMenuItem>
                ) : null}
              </DropdownMenuGroup>
            </> : null}
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
                <DropdownMenuItem disabled={removal?.in_flight === true} onClick={() => void closing.start(true)}>
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
          <SyncDialog organizationSlug={params.organizationSlug} from={store} into={into}
            // Closing after syncs only a Branch that isn't kept, and only into its Parent; a PR Environment closes
            // with its pull request.
            closable={into === branch.parent && !branch.kept && !me?.default && !shuttable}
            onClose={() => setInto(null)} onSynced={(changes, sync) => synced(into, changes, sync)} />
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
