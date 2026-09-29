import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote, prefetchStoreViews } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { branchPlanQuery } from "#/modules/config-store/store-view.queries";
import { deploymentAttemptQueryOptions } from "#/modules/deployments/deployment-history.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { NewBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/NewBranchPanel";
import { StoreNewBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/StoreNewBranchPanel";

/**
 * "New branch of X": a panel over the Parent's canvas. `focus` names the lineage that changes; `fix` names the failed
 * attempt whose change to that lineage the Branch carries (Fix it on a branch).
 */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/new-branch",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ focus: Schema.optional(Schema.String), fix: Schema.optional(Schema.String) })),
  loaderDeps: ({ search }) => ({ fix: search.fix, focus: search.focus }),
  loader: ({ params, context, deps }) => storeEnabled
    // Over the Store, `focus` names a Service, and the panel opens on the Store's first plan.
    ? prefetchStoreViews(context, params.organizationSlug, branchPlanQuery(
      { project: params.projectSlug, environment: params.environmentSlug }, deps.focus ? [deps.focus] : [], { preset: "only" }))
    // The failed attempt names the change the Branch carries.
    : deps.fix ? prefetchRemote(context, deploymentAttemptQueryOptions(params.organizationSlug, deps.fix)) : undefined,
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="New branch" />,
  component: RouteComponent,
});

// The scene's BranchPickingProvider reads `focus`; the panel and the canvas share its picks.
function RouteComponent() {
  const { focus, fix } = Route.useSearch();
  return storeEnabled
    ? <StoreNewBranchPanel focus={focus ?? null} fix={fix ?? null} />
    : <NewBranchPanel focus={focus ?? null} fix={fix ?? null} />;
}
