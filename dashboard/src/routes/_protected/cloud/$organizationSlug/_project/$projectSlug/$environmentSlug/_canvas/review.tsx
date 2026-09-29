import { createFileRoute } from "@tanstack/react-router";
import { prefetchStoreViews } from "#/collections/route-data";
import { branchQuery, environmentsQuery, saveQuery, updateQuery } from "#/modules/config-store/store-view.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { StoreBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/branch-review/StoreBranchPanel";

/** A Branch's Manage panel: everything about the Branch, over its canvas. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/review",
)({
  loader: async ({ params, context }) => {
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    await prefetchStoreViews(context, params.organizationSlug, branchQuery(store), saveQuery(store), updateQuery(store), environmentsQuery(params.projectSlug));
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Branch" />,
  component: StoreBranchPanel,
});
