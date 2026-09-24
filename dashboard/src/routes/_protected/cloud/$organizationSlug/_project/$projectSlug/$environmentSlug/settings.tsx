import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { prefetchRemote, requireEnvironment } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import { Separator } from "#/components/ui/separator";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { Route as EnvironmentLayoutRoute } from "./route";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings",
)({
  loader: async ({ params, context }) => {
    const { organizationSlug, projectSlug } = params;
    const environment = await requireEnvironment(context, params);
    await prefetchRemote(context,
      latestTeardownAttemptQueryOptions({ organizationSlug, scope: "environment", environmentId: environment.id }),
      latestTeardownAttemptQueryOptions({ organizationSlug, scope: "project", projectSlug }));
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, projectSlug, environmentSlug } = Route.useParams();
  const { environmentId } = EnvironmentLayoutRoute.useLoaderData();
  const navigate = useNavigate();
  const { projects, environments } = useWorkspace(organizationSlug);
  const project = projects.find((row) => row.slug === projectSlug);
  const environment = environments.find((row) => row.id === environmentId);

  function leaveDeletedTree() {
    void navigate({
      to: "/cloud/$organizationSlug/~",
      params: { organizationSlug },
      replace: true,
    });
  }

  return (
    <DashboardPage width="content">
      <h1 className="sr-only">Environment settings</h1>
      <div className="flex flex-col gap-8">
        <TeardownDangerSection
          organizationSlug={organizationSlug}
          scope="environment"
          environmentId={environmentId}
          confirmPhrase={environment?.name ?? environmentSlug}
          title="Tear down this environment"
          description="Deletes this environment and all of its services and volumes. This cannot be undone."
          actionLabel="Tear down environment"
          headingId="environment-teardown-heading"
          onCompleted={leaveDeletedTree}
        />
        <Separator />
        <TeardownDangerSection
          organizationSlug={organizationSlug}
          scope="project"
          projectSlug={projectSlug}
          confirmPhrase={project?.name ?? projectSlug}
          title="Tear down this project"
          description="Deletes this project and all of its environments, services, and volumes. This cannot be undone."
          actionLabel="Tear down project"
          headingId="project-teardown-heading"
          showHeading={false}
          onCompleted={leaveDeletedTree}
        />
      </div>
    </DashboardPage>
  );
}
