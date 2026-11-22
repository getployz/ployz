import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { requireStoreEnvironment } from "#/modules/config-store/store.route-data";
import { streamRemoteUptime } from "#/modules/uptime/uptime.route-data";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { serviceSearchSchema } from "./$serviceId/-components/service-pages";
import { StoreServiceDrawer } from "./$serviceId/-components/StoreServiceDrawer";
import { DEFAULT_RANGE } from "#/lib/time-range";
import { serviceUptimeQueryOptions } from "#/modules/uptime/uptime.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId",
)({
  validateSearch: Schema.toStandardSchemaV1(serviceSearchSchema),
  loaderDeps: ({ search }) => ({ tab: search.tab, from: search.from, to: search.to }),
  loader: async ({ params, context, deps }) => {
    if (deps.tab === "monitoring") {
      const environment = await requireStoreEnvironment(context, params);
      const scope = { organizationSlug: params.organizationSlug, environmentId: environment.id };
      streamRemoteUptime(context, params.organizationSlug, serviceUptimeQueryOptions(scope, params.serviceId, { from: deps.from ?? DEFAULT_RANGE.from, to: deps.to ?? DEFAULT_RANGE.to }));
    }
  },
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Service" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StoreServiceDrawer params={Route.useParams()} />;
}
