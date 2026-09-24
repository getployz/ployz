import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { createFileRoute } from "@tanstack/react-router";
import { RocketIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { DeploymentRow } from "#/components/deployment-row";
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

  const { data: rows } = useLiveSuspenseQuery(deployments);

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
