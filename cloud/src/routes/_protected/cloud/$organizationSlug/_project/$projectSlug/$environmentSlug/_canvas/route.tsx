import { preloadCollection } from "#/collections/query-collection";
import {
  Await,
  createFileRoute,
  type ErrorComponentProps,
} from "@tanstack/react-router";
import { RouteErrorAlert } from "#/components/route-error-alert";
import {
  getCanvasPositionsCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getEnvironmentNodeIntroductionsCollection,
  getVolumeRemoveAttemptsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getRawEnvironmentResourcesCollection,
  getRawServicesCollection,
  getResourceLineagesCollection,
} from "#/collections/collections";
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
    const scope = { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    const canvasReady = Promise.all([
      preloadCollection(getProjectsCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope)),
      preloadCollection(getVolumeRemoveAttemptsCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentsCollection(organizationSlug, scope)),
      preloadCollection(getRawServicesCollection(organizationSlug, scope)),
      preloadCollection(getCanvasPositionsCollection(organizationSlug, scope)),
      preloadCollection(getRawEnvironmentResourcesCollection(organizationSlug, scope)),
      preloadCollection(getResourceLineagesCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentNodeIntroductionsCollection(organizationSlug, scope)),
      preloadOrganizationEnvironmentChangeStateProjections(
        scope,
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
