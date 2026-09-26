import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote } from "#/collections/route-data";
import { deploymentBuildLogQueryOptions } from "#/modules/deployments/deployment-build-log.queries";
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
  loaderDeps: ({ search }) => ({ deployment: search.deployment, tab: search.tab }),
  // Deployment Mode's Build logs tab reads the whole log; SSR renders it, and hovering a node warms it.
  loader: async ({ params, context, deps }) => {
    if (deps.deployment && deps.tab === "build-logs") await prefetchRemote(context, deploymentBuildLogQueryOptions(params.organizationSlug, deps.deployment));
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
