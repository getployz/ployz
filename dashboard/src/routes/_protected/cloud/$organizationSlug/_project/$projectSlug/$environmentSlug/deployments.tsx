import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
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
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useDeploymentsCollection } from "#/modules/services/services.collection";
import { Route as EnvironmentLayoutRoute } from "./route";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments",
)({
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
  const { environmentId } = EnvironmentLayoutRoute.useLoaderData();
  const workspace = useWorkspace(params.organizationSlug);
  const projectName = workspace.projects.find((project) => project.slug === projectSlug)?.name ?? projectSlug;
  const environmentName = workspace.environments.find((environment) => environment.id === environmentId)?.name ?? environmentSlug;
  const deployments = useDeploymentsCollection(params.organizationSlug);

  const { data: rows } = useLiveSuspenseQuery({
    queryKey: ['environment-deployments', deployments.id, projectSlug, environmentSlug],
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
