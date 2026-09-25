import { Suspense } from "react";
import {
  createFileRoute,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { prefetchRemote } from "#/collections/route-data";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { deploymentBuildTailQueryOptions } from "#/modules/deployments/deployment-build-log.queries";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";
import { canvasRouteSearch } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-mode";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  ...canvasRouteSearch,
  loaderDeps: ({ search }) => ({ deployment: search.deployment }),
  // Deployment Mode's nodes read the build tail; SSR renders them with it and hover preload warms it.
  loader: async ({ params, context, deps }) => {
    if (deps.deployment) await prefetchRemote(context, deploymentBuildTailQueryOptions(params.organizationSlug, deps.deployment));
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
