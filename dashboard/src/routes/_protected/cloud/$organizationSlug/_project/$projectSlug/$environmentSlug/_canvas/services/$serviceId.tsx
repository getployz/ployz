import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchStoreViews, requireStoreEnvironment, streamRemote } from "#/collections/route-data";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { serviceSearchSchema } from "../../services/$serviceId/-components/service-pages";
import { StoreServiceDrawer } from "../../services/$serviceId/-components/StoreServiceDrawer";
import { domainsQuery } from "#/modules/config-store/store-view.queries";
import { serviceUptimeQueryOptions } from "#/modules/uptime/uptime.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId",
)({
  validateSearch: Schema.toStandardSchemaV1(serviceSearchSchema),
  loaderDeps: ({ search }) => ({ tab: search.tab, range: search.range }),
  loader: async ({ params, context, deps }) => {
    if (deps.tab === "monitoring") {
      const environment = await requireStoreEnvironment(context, params);
      const scope = { organizationSlug: params.organizationSlug, environmentId: environment.id };
      streamRemote(context, serviceUptimeQueryOptions(scope, params.serviceId, deps.range ?? "24h"));
    }
    await prefetchStoreViews(context, params.organizationSlug, domainsQuery({ project: params.projectSlug, environment: params.environmentSlug }));
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Service" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StoreServiceDrawer params={Route.useParams()} />;
}
