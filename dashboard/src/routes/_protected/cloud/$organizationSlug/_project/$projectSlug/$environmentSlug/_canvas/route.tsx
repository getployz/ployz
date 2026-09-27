import { Suspense } from "react";
import {
  createFileRoute,
  redirect,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchFromOrgStore, prefetchRemotePages, requireEnvironment } from "#/collections/route-data";
import { environmentDeploymentsQueryOptions } from "#/modules/deployments/deployment-history.queries";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";
import { DEPLOYMENT_PAGE_ROUTE_TO, legacyDeploymentLink } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-page";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  // `deploymentList=true` opens the deploy bar's deployment list; it is not retained.
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ deploymentList: Schema.optional(Schema.Boolean) })),
  // Old Deployment Mode links (`?deployment=<id>`, on the canvas or a service) open that attempt's Deployment Page.
  beforeLoad: ({ location, params }) => {
    const legacy = legacyDeploymentLink(location);
    if (legacy) throw redirect({ to: DEPLOYMENT_PAGE_ROUTE_TO, params: { ...params, deploymentId: legacy.deploymentId }, search: legacy.search, replace: true });
  },
  loaderDeps: ({ search }) => ({ deploymentList: search.deploymentList }),
  // The open list's first page; the list is keyed by environment id, which the Org Store resolves.
  loader: async ({ params, context, deps: { deploymentList } }) => {
    const { organizationSlug } = params;
    await prefetchFromOrgStore(context, organizationSlug, () => [
      deploymentList === true && requireEnvironment(context, params).then((environment) =>
        prefetchRemotePages(context, environmentDeploymentsQueryOptions(organizationSlug, environment.id))),
    ]);
  },
  errorComponent: CanvasError,
  component: CanvasLayout,
});

function CanvasLayout() {
  return (
    <div className="h-full overflow-hidden">
      <Suspense fallback={<PendingCanvas />}>
        <EnvironmentCanvasScene />
      </Suspense>
    </div>
  );
}

function CanvasError({ error }: ErrorComponentProps) {
  return (
    <div className="flex h-full items-center justify-center p-6">
      <RouteErrorAlert
        title="Couldn’t load this environment"
        description={
          error.message || "The environment data could not be loaded."
        }
      />
    </div>
  );
}
