import { preloadCollection } from "#/collections/query-collection";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { createFileRoute } from "@tanstack/react-router";
import { RocketIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { DeploymentRow } from "#/components/deployment-row";
import { DeploymentHistorySkeleton } from "#/components/deployment-history-skeleton";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";
import {
  getEnvironmentDeploymentsCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getRawEnvironmentResourcesCollection,
  getVolumeRemoveAttemptsCollection,
} from "#/collections/collections";
import { useDeploymentsCollection } from "#/modules/services/services.collection";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments",
)({
  loader: async ({ params, context }) => {
    const organizationSlug = params.organizationSlug;
    const scope = { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    await Promise.all([
      preloadCollection(getProjectsCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentsCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentDeploymentsCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope)),
      preloadCollection(getRawEnvironmentResourcesCollection(organizationSlug, scope)),
      preloadCollection(getVolumeRemoveAttemptsCollection(organizationSlug, scope)),
    ]);
    return {
      projectName: [...getProjectsCollection(organizationSlug, scope).values()].find((project) => project.slug === params.projectSlug)?.name ?? params.projectSlug,
      environmentName: [...getEnvironmentsCollection(organizationSlug, scope).values()].find((environment) => environment.namespace === params.environmentSlug)?.name ?? params.environmentSlug,
    };
  },
  pendingComponent: () => <DashboardPage density="compact" width="content"><DeploymentHistorySkeleton /></DashboardPage>,
  component: RouteComponent,
});

function RouteComponent() {
  return (
    <DashboardPage density="compact" width="content">
      <DeploymentHistory />
    </DashboardPage>
  );
}

function DeploymentHistory() {
  const params = Route.useParams();
  const { projectSlug, environmentSlug } = params;
  const { projectName, environmentName } = Route.useLoaderData();
  const deployments = useDeploymentsCollection(params.organizationSlug);

  const { data: rows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ deployment: deployments })
        .where(({ deployment }) => eq(deployment.projectSlug, projectSlug))
        .where(({ deployment }) =>
          eq(deployment.environmentSlug, environmentSlug),
        )
        .select(({ deployment }) => deployment),
  });

  const sorted = [...rows].sort(
    (left, right) => right.createdAt.getTime() - left.createdAt.getTime(),
  );

  if (sorted.length === 0) {
    return (
      <Empty variant="first-run">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <RocketIcon />
          </EmptyMedia>
          <EmptyTitle>{projectName} / {environmentName} has no deployments yet</EmptyTitle>
          <EmptyDescription>
            Deploy a service to see deployment history here.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  }

  return (
    <>
      {sorted.map((deployment) => (
        <DeploymentRow key={deployment.id} deployment={deployment} />
      ))}
    </>
  );
}
