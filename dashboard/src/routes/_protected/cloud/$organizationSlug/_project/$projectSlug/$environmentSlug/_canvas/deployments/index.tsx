import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchFromOrgStore, prefetchRemotePages, requireEnvironment } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { environmentDeploymentsQueryOptions, nodeDeploymentsQueryOptions } from "#/modules/deployments/deployment-history.queries";
import {
  CanvasInspectorError,
  CanvasInspectorPending,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorRouteStates";
import { DeploymentsList } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/DeploymentsList";

/** The Environment's Cloud Deployment Attempts, newest first: a panel over the live canvas. `service` narrows it to one. */
export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ service: Schema.optional(Schema.String) })),
  loaderDeps: ({ search }) => ({ service: search.service }),
  // The first page of whichever history shows; both are keyed by environment id, which the Org Store resolves. Over the
  // Store, the Environment's loader prefetches its Deployments' first page for the bottom bar, and this list is that read.
  loader: ({ params, context, deps: { service } }) => storeEnabled ? undefined : prefetchFromOrgStore(context, params.organizationSlug, () => [
    requireEnvironment(context, params).then((environment) => service
      ? prefetchRemotePages(context, nodeDeploymentsQueryOptions(params.organizationSlug, environment.id, service))
      : prefetchRemotePages(context, environmentDeploymentsQueryOptions(params.organizationSlug, environment.id))),
  ]),
  pendingComponent: CanvasInspectorPending,
  errorComponent: () => <CanvasInspectorError noun="Deployments" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <DeploymentsList service={Route.useSearch().service ?? null} />;
}
