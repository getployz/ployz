import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { DeploymentsList } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/DeploymentsList";
import { DeploymentsPageError, DeploymentsPagePending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/DeploymentsPageStates";

/**
 * The Environment's Runs, newest first. `service` narrows them to one. The Environment's loader prefetched their first
 * page for the bottom bar, and this list is that read.
 */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments/")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ service: Schema.optional(Schema.String) })),
  pendingComponent: DeploymentsPagePending,
  errorComponent: () => <DeploymentsPageError noun="Runs" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <DeploymentsList service={Route.useSearch().service ?? null} />;
}
