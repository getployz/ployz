import { preloadCollection } from "#/collections/query-collection";
import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { createFileRoute } from "@tanstack/react-router";
import { RocketIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { DeploymentRow } from "#/components/deployment-row";
import { DeploymentHistorySkeleton } from "#/components/deployment-history-skeleton";
import {
  getEnvironmentDeploymentsCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getRawEnvironmentResourcesCollection,
  getVolumeRemoveAttemptsCollection,
} from "#/collections/collections";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";
import { useDeploymentsCollection } from "#/modules/services/services.collection";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/deployments",
)({
  loader: async ({ params, context }) => {
    const organizationSlug = params.organizationSlug;
    const scope = { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    await Promise.all([
      preloadCollection(getEnvironmentDeploymentsCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentsCollection(organizationSlug, scope)),
      preloadCollection(getProjectsCollection(organizationSlug, scope)),
      preloadCollection(getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope)),
      preloadCollection(getRawEnvironmentResourcesCollection(organizationSlug, scope)),
      preloadCollection(getVolumeRemoveAttemptsCollection(organizationSlug, scope)),
    ]);
  },
  pendingComponent: () => <DashboardPage density="compact"><DeploymentHistorySkeleton /></DashboardPage>,
  component: RouteComponent,
});

function RouteComponent() {
  return (
    <DashboardPage density="compact">
      <DeploymentHistory />
    </DashboardPage>
  );
}

function DeploymentHistory() {
  const { organizationSlug } = Route.useParams();
  const deployments = useDeploymentsCollection(organizationSlug);

  const { data: rows } = useLiveSuspenseQuery({
    query: (q) =>
      q.from({ deployment: deployments }).select(({ deployment }) => deployment),
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
          <EmptyTitle>No deployments yet</EmptyTitle>
          <EmptyDescription>
            Deploy a service to start seeing deployment history here.
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
