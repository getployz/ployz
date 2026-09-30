import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchStoreViews } from "#/collections/route-data";
import { deploymentQuery } from "#/modules/config-store/store-view.queries";
import { StoreDeploymentPage } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/StoreDeploymentPage";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { deploymentPageSearchSchema } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-page";

/** A Deployment's page: a panel over the live canvas, which stays mounted underneath. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/$deploymentId",
)({
  validateSearch: Schema.toStandardSchemaV1(deploymentPageSearchSchema),
  // The Deployment (Node Outcomes, builds); a build log loads as its tab shows.
  loader: ({ params, context }) => prefetchStoreViews(context, params.organizationSlug, deploymentQuery(params.deploymentId)),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Deployment" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StoreDeploymentPage deploymentId={Route.useParams().deploymentId} search={Route.useSearch()} />;
}
