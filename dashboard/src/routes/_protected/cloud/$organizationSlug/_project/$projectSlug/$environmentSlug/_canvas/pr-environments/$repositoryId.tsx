import { createFileRoute } from "@tanstack/react-router";
import { prefetchRemote } from "#/collections/route-data";
import { missingPrEnvironmentGrantsQueryOptions } from "#/modules/pr-environments/plan.queries";
import { CanvasInspectorError, CanvasInspectorPending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { PrPlanPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/pr-environments/PrPlanPanel";

/** A repository's PR Environments plan: a panel over its start-from Environment's canvas. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/pr-environments/$repositoryId",
)({
  loader: ({ params, context }) => prefetchRemote(context, missingPrEnvironmentGrantsQueryOptions(params.organizationSlug)),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="PR environments" />,
  component: RouteComponent,
});

function RouteComponent() {
  const { repositoryId } = Route.useParams();
  return <PrPlanPanel repositoryId={Number(repositoryId)} />;
}
