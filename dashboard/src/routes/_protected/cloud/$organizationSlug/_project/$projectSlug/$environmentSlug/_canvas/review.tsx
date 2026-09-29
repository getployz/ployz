import { createFileRoute } from "@tanstack/react-router";
import { prefetchRemote, prefetchStoreViews, requireEnvironment } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { branchQuery, environmentsQuery, saveQuery, updateQuery } from "#/modules/config-store/store-view.queries";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { BranchReviewPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/branch-review/BranchReviewPanel";
import { StoreBranchPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/branch-review/StoreBranchPanel";

/** A Branch's Manage panel: everything about the Branch, over its canvas. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/review",
)({
  loader: async ({ params, context }) => {
    // Over the Store the Environment's page read it already; Cloud keeps no row for it.
    if (storeEnabled) {
      const store = { project: params.projectSlug, environment: params.environmentSlug };
      await prefetchStoreViews(context, params.organizationSlug, branchQuery(store), saveQuery(store), updateQuery(store), environmentsQuery(params.projectSlug));
      return;
    }
    const environment = await requireEnvironment(context, params);
    await prefetchRemote(context, latestTeardownAttemptQueryOptions({ organizationSlug: params.organizationSlug, scope: "environment", environmentId: environment.id }));
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Branch" />,
  component: RouteComponent,
});

function RouteComponent() {
  return storeEnabled ? <StoreBranchPanel /> : <BranchReviewPanel />;
}
