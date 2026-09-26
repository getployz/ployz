import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote, prefetchRemotePages, requireEnvironment } from "#/collections/route-data";
import { deploymentBuildLogQueryOptions } from "#/modules/deployments/deployment-build-log.queries";
import { deploymentAttemptQueryOptions, nodeDeploymentsQueryOptions } from "#/modules/deployments/deployment-history.queries";
import { viewedDeploymentId } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-mode";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { ServiceDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceDrawer";
import { useServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";
import { serviceSearchSchema } from "../../services/$serviceId/-components/service-pages";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId",
)({
  validateSearch: Schema.toStandardSchemaV1(serviceSearchSchema),
  // Switching tabs navigates, so the open tab's read starts as it opens: the Deployments tab's first page, or in Deployment
  // Mode the configs the attempt deployed (Details) or the whole build log (Build logs). SSR renders them; hovering a node warms logs.
  loaderDeps: ({ search }) => ({ deployment: viewedDeploymentId(search), tab: search.tab }),
  loader: async ({ params, context, deps }) => {
    if (deps.deployment !== null && deps.tab === "details") await prefetchRemote(context, deploymentAttemptQueryOptions(params.organizationSlug, deps.deployment));
    if (deps.deployment !== null && deps.tab === "build-logs") await prefetchRemote(context, deploymentBuildLogQueryOptions(params.organizationSlug, deps.deployment));
    if (deps.tab !== "deployments") return;
    const environment = await requireEnvironment(context, params);
    await prefetchRemotePages(context, nodeDeploymentsQueryOptions(params.organizationSlug, environment.id, params.serviceId));
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Service" />,
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const state = useServiceDrawerState(params);

  if (state == null) {
    return null;
  }

  return <ServiceDrawer params={params} state={state} />;
}
