import { createFileRoute, useLoaderData } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote, prefetchStoreConfig } from "#/collections/route-data";
import { configsQuery, requireView, useStoreViews } from "#/modules/config-store/store-view.queries";
import { volumePlacementsQueryOptions } from "#/modules/volume-placement/volume-placement.queries";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { ENVIRONMENT_ROUTE_FROM } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import { StoreConfigDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/StoreConfigDrawer";
import { StoreVolumeDrawer } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/StoreVolumeDrawer";
import { volumeSearchSchema } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/volume-pages";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId",
)({
  validateSearch: Schema.toStandardSchemaV1(volumeSearchSchema),
  loader: ({ params, context }) => Promise.all([
    prefetchStoreConfig(context, params.organizationSlug,
      { project: params.projectSlug, environment: params.environmentSlug }, params.resourceId),
    prefetchRemote(context, volumePlacementsQueryOptions(params.organizationSlug)),
  ]),
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
