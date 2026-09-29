import { createFileRoute } from "@tanstack/react-router";
import { prefetchRemote, prefetchRemoteWithStoreViews } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { prPlansQuery } from "#/modules/config-store/store-pull-requests";
import { environmentsQuery } from "#/modules/config-store/store-view.queries";
import { missingPrEnvironmentGrantsQueryOptions, missingStorePrGrantsQueryOptions } from "#/modules/pr-environments/plan.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { PrPlanPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/pr-environments/PrPlanPanel";
import { StorePrPlanPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/pr-environments/StorePrPlanPanel";

/** A repository's PR Environments plan: a panel over its start-from Environment's canvas. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/pr-environments/$repositoryId",
)({
  loader: async ({ params, context }) => {
    const { organizationSlug, projectSlug } = params;
    if (!storeEnabled) return prefetchRemote(context, missingPrEnvironmentGrantsQueryOptions(organizationSlug));
    // The start-from's Services, Volumes and Settings come with the Environment's own loader.
    await prefetchRemoteWithStoreViews(context, organizationSlug, [prPlansQuery(projectSlug), environmentsQuery(projectSlug)],
      missingStorePrGrantsQueryOptions(organizationSlug, projectSlug));
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="PR environments" />,
  component: RouteComponent,
});

function RouteComponent() {
  const { repositoryId } = Route.useParams();
  // TODO(#1275): the legacy panel goes with the dark gate.
  return storeEnabled ? <StorePrPlanPanel repositoryId={Number(repositoryId)} /> : <PrPlanPanel repositoryId={Number(repositoryId)} />;
}
