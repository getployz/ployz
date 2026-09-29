import { createFileRoute } from "@tanstack/react-router";
import { prefetchRemoteWithStoreViews } from "#/collections/route-data";
import { prPlansQuery } from "#/modules/config-store/store-pull-requests";
import { missingStorePrGrantsQueryOptions } from "#/modules/config-store/store-pr-grants.queries";
import { environmentsQuery } from "#/modules/config-store/store-view.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { StorePrPlanPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/pr-environments/StorePrPlanPanel";

/** A repository's PR Environments plan: a panel over its start-from Environment's canvas. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/pr-environments/$repositoryId",
)({
  // The start-from's Services, Volumes and Settings come with the Environment's own loader.
  loader: ({ params, context }) => prefetchRemoteWithStoreViews(context, params.organizationSlug,
    [prPlansQuery(params.projectSlug), environmentsQuery(params.projectSlug)],
    missingStorePrGrantsQueryOptions(params.organizationSlug, params.projectSlug)),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="PR environments" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StorePrPlanPanel repositoryId={Number(Route.useParams().repositoryId)} />;
}
