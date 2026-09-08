import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { Await, createFileRoute } from "@tanstack/react-router";
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
} from "#/electric/collections";
import { useDeploymentsCollection } from "#/modules/services/services.collection";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments",
)({
  loader: ({ params, context }) => {
    const organizationSlug = params.organizationSlug;
    const baseUrl = context.tableSyncBaseUrl;
    const deploymentsReady = Promise.all([
      getProjectsCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentsCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentDeploymentsCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentNodeConfigSnapshotsCollection(
        organizationSlug,
        baseUrl,
      ).preload(),
      getRawEnvironmentResourcesCollection(organizationSlug, baseUrl).preload(),
      getVolumeRemoveAttemptsCollection(
        organizationSlug,
        baseUrl,
      ).preload(),
    ]);

    return { deploymentsReady };
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { deploymentsReady } = Route.useLoaderData();

  return (
    <DashboardPage density="compact" width="content">
      <Await
        promise={deploymentsReady}
        fallback={<DeploymentHistorySkeleton />}
      >
        {() => <DeploymentHistory />}
      </Await>
    </DashboardPage>
  );
}

function DeploymentHistory() {
  const params = Route.useParams();
  const { projectSlug, environmentSlug } = params;
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
          <EmptyTitle>No deployments yet</EmptyTitle>
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
