import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemotePages, requireEnvironment } from "#/collections/route-data";
import { nodeDeploymentsQueryOptions } from "#/modules/deployments/deployment-history.queries";
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
  // Switching tabs navigates, so the Deployments tab's first page starts as it opens. SSR renders it.
  loaderDeps: ({ search }) => ({ tab: search.tab }),
  loader: async ({ params, context, deps }) => {
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
