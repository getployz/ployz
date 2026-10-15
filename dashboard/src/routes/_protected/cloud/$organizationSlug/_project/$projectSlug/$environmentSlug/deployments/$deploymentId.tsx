import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchStoreViews } from "#/collections/route-data";
import { deploymentQuery } from "#/modules/config-store/store-view.queries";
import { StoreDeploymentPage } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/StoreDeploymentPage";
import { DeploymentsPageError, DeploymentsPagePending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/DeploymentsPageStates";
import { runSearchSchema } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/run-search";

/** One Deployment's Run. `job` picks the build or node its panel shows, `origin` is the Architecture node its back returns to. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments/$deploymentId")({
  validateSearch: Schema.toStandardSchemaV1(runSearchSchema),
  // The Deployment (Node Outcomes, builds); a build log loads as its panel shows.
  loader: ({ params, context }) => prefetchStoreViews(context, params.organizationSlug, deploymentQuery(params.deploymentId)),
  pendingComponent: DeploymentsPagePending,
  errorComponent: () => <DeploymentsPageError noun="Run" />,
  component: RouteComponent,
});

function RouteComponent() {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <StoreDeploymentPage deploymentId={Route.useParams().deploymentId} search={Route.useSearch()} />
    </div>
  );
}
