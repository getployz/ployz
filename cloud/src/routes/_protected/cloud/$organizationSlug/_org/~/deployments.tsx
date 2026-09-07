import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { Await, createFileRoute } from "@tanstack/react-router";
import { RocketIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { DeploymentRow } from "#/components/deployment-row";
import { DeploymentHistorySkeleton } from "#/components/deployment-history-skeleton";
import {
  getDestructiveVolumeAttemptsCollection,
  getEnvironmentDeploymentsCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
} from "#/electric/collections";
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
  loader: ({ params, context }) => {
    const organizationSlug = params.organizationSlug;
    const baseUrl = context.tableSyncBaseUrl;
    const deploymentsReady = Promise.all([
      getEnvironmentDeploymentsCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentsCollection(organizationSlug, baseUrl).preload(),
      getProjectsCollection(organizationSlug, baseUrl).preload(),
      getEnvironmentNodeConfigSnapshotsCollection(
        organizationSlug,
        baseUrl,
      ).preload(),
      getDestructiveVolumeAttemptsCollection(
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
    <DashboardPage density="compact">
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
