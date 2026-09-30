import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchStoreViews } from "#/collections/route-data";
import { branchPlanQuery } from "#/modules/config-store/store-view.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { StoreNewBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/StoreNewBranchPanel";

/**
 * "New branch of X": a panel over the Parent's canvas. `focus` names the Service that changes; `fix` names the failed
 * Deployment whose change to it the Branch carries (Fix it on a branch).
 */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/new-branch",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ focus: Schema.optional(Schema.String), fix: Schema.optional(Schema.String) })),
  loaderDeps: ({ search }) => ({ focus: search.focus }),
  // The panel opens on the Store's first plan.
  loader: ({ params, context, deps }) => prefetchStoreViews(context, params.organizationSlug, branchPlanQuery(
    { project: params.projectSlug, environment: params.environmentSlug }, deps.focus ? [deps.focus] : [], { preset: "only" })),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="New branch" />,
  component: RouteComponent,
});

// The scene's picking provider reads `focus`; the panel and the canvas share its picks.
function RouteComponent() {
  const { focus, fix } = Route.useSearch();
  return <StoreNewBranchPanel focus={focus ?? null} fix={fix ?? null} />;
}
