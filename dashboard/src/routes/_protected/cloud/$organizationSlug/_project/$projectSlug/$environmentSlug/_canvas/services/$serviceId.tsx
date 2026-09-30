import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemoteWithStoreViews } from "#/collections/route-data";
import { customDomainCapabilityQueryOptions } from "#/modules/billing/billing.queries";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { serviceSearchSchema } from "../../services/$serviceId/-components/service-pages";
import { StoreServiceDrawer } from "../../services/$serviceId/-components/StoreServiceDrawer";
import { domainsQuery } from "#/modules/config-store/store-view.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId",
)({
  validateSearch: Schema.toStandardSchemaV1(serviceSearchSchema),
  loader: ({ params, context }) =>
    prefetchRemoteWithStoreViews(context, params.organizationSlug,
      [domainsQuery({ project: params.projectSlug, environment: params.environmentSlug })],
      customDomainCapabilityQueryOptions(params.organizationSlug)),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Service" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <StoreServiceDrawer params={Route.useParams()} />;
}
