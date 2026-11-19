import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchStoreEnvironment } from "#/collections/route-data";
import { servicesQuery } from "#/modules/config-store/store-view.queries";
import { DeploymentsList } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/DeploymentsList";
import { DeploymentsPageError, DeploymentsPagePending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/DeploymentsPageStates";

/** The Environment's Runs, newest first. `service` narrows them to one, from the Environment's Services. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments/")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ service: Schema.optional(Schema.String) })),
  loader: ({ params, context }) => {
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    return prefetchStoreEnvironment(context, params.organizationSlug, store, [servicesQuery(store)]);
  },
  pendingComponent: DeploymentsPagePending,
  errorComponent: () => <DeploymentsPageError noun="Runs" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <DeploymentsList service={Route.useSearch().service ?? null} />;
}
