import { Suspense } from "react";
import {
  createFileRoute,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { getEnvironmentDeploymentsCollection } from "#/collections/collections";
import { prefetchFromOrgStore, prefetchRemote, prefetchRemotePages, requireEnvironment } from "#/collections/route-data";
import { deploymentBuildTailQueryOptions } from "#/modules/deployments/deployment-build-log.queries";
import { deploymentAttemptQueryOptions, environmentDeploymentsQueryOptions } from "#/modules/deployments/deployment-history.queries";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";
import { canvasRouteSearch, viewedDeploymentId } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-mode";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  ...canvasRouteSearch,
  loaderDeps: ({ search }) => ({ deployment: viewedDeploymentId(search), deploymentList: search.deploymentList }),
  // Deployment Mode's nodes read the attempt and its build tail, and the open list its first page; all start together.
  // SSR renders them and hover preload warms them.
  loader: async ({ params, context, deps: { deployment, deploymentList } }) => {
    const { organizationSlug } = params;
    await prefetchFromOrgStore(context, organizationSlug, (scope) => [
      deployment !== null && prefetchRemote(context, deploymentBuildTailQueryOptions(organizationSlug, deployment)),
      // An attempt the Org Store holds draws from it alone.
      deployment !== null && !getEnvironmentDeploymentsCollection(organizationSlug, scope).has(deployment)
        && prefetchRemote(context, deploymentAttemptQueryOptions(organizationSlug, deployment)),
      // The list is keyed by environment id, which the Org Store resolves.
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
