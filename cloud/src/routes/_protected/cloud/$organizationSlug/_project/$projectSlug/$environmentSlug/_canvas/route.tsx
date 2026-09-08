import {
  Await,
  createFileRoute,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  getCanvasPositionsCollection,
  getDestructiveVolumeAttemptsCollection,
  getEnvironmentNodeIntroductionsCollection, getEnvironmentNodeConfigSnapshotsCollection, getVolumeRemovalResultsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getRawEnvironmentResourcesCollection,
  getRawServicesCollection,
  getResourceLineagesCollection,
} from "#/electric/collections";
import { preloadOrganizationEnvironmentChangeStateProjections } from "#/modules/deployments/use-environment-state-projection";
import {
  EnvironmentCanvasScene,
  PendingCanvas,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentCanvasScene";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas",
)({
  loader: ({ params, context }) => {
    const organizationSlug = params.organizationSlug;
    const baseUrl = context.tableSyncBaseUrl;
    const canvasReady = Promise.all([
      getProjectsCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, baseUrl).preload(),
      getVolumeRemovalResultsCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentsCollection(organizationSlug, baseUrl).preload(),
      getRawServicesCollection(organizationSlug, baseUrl).preload(),
      getCanvasPositionsCollection(organizationSlug, baseUrl).preload(),
      getRawEnvironmentResourcesCollection(
        organizationSlug,
        baseUrl,
      ).preload(),
      getResourceLineagesCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentNodeIntroductionsCollection(
        organizationSlug,
        baseUrl,
      ).preload(),
      getDestructiveVolumeAttemptsCollection(
        organizationSlug,
        baseUrl,
      ).preload(),
      preloadOrganizationEnvironmentChangeStateProjections(
        context.queryClient,
        organizationSlug,
      ),
    ]);

    return { canvasReady };
  },
  errorComponent: CanvasError,
  component: CanvasLayout,
});

function CanvasLayout() {
  const { canvasReady } = Route.useLoaderData();

  return (
    <div className="h-full overflow-hidden">
      <Await promise={canvasReady} fallback={<PendingCanvas />}>
        {() => <EnvironmentCanvasScene />}
      </Await>
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
