import { createFileRoute } from "@tanstack/react-router";
import { prefetchRemote, requireEnvironment } from "#/collections/route-data";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { BranchReviewPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/branch-review/BranchReviewPanel";

/** A Branch's Manage panel: everything about the Branch, over its canvas. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/review",
)({
  loader: async ({ params, context }) => {
    const environment = await requireEnvironment(context, params);
    await prefetchRemote(context, latestTeardownAttemptQueryOptions({ organizationSlug: params.organizationSlug, scope: "environment", environmentId: environment.id }));
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Branch" />,
  component: BranchReviewPanel,
});
