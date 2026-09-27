import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote } from "#/collections/route-data";
import { deploymentBuildLogQueryOptions, deploymentBuildTailQueryOptions } from "#/modules/deployments/deployment-build-log.queries";
import { deploymentAttemptQueryOptions } from "#/modules/deployments/deployment-history.queries";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { DeploymentPage } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/DeploymentPage";
import { deploymentPageSearchSchema } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-page";

/** A Cloud Deployment Attempt's Deployment Page: a panel over the live canvas, which stays mounted underneath. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/$deploymentId",
)({
  validateSearch: Schema.toStandardSchemaV1(deploymentPageSearchSchema),
  // The attempt (who started it, the configs it deployed), its build tail (each image's stage) and its build log (the Build
  // tab) start together; SSR renders them and hover warms them. The deploy logs stream once the Deploy tab shows.
  loader: ({ params, context }) => prefetchRemote(context,
    deploymentAttemptQueryOptions(params.organizationSlug, params.deploymentId),
    deploymentBuildTailQueryOptions(params.organizationSlug, params.deploymentId),
    deploymentBuildLogQueryOptions(params.organizationSlug, params.deploymentId)),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Deployment" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <DeploymentPage deploymentId={Route.useParams().deploymentId} search={Route.useSearch()} />;
}
