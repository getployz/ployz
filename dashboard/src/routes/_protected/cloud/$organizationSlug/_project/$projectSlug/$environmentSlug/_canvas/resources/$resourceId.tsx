import { createFileRoute, useLoaderData } from "@tanstack/react-router";
import { configsQuery, requireView, useStoreViews } from "#/modules/config-store/store-view.queries";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { ENVIRONMENT_ROUTE_FROM } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import { StoreConfigDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/StoreConfigDrawer";
import { StoreVolumeDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/StoreVolumeDrawer";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId",
)({
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Resource" />,
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const [configs] = useStoreViews(params.organizationSlug, [configsQuery(store)] as const);
  const config = requireView(configs).configs.find((candidate) => candidate.id === params.resourceId);
  return config ? <StoreConfigDrawer params={params} config={config} /> : <StoreVolumeDrawer params={params} />;
}
