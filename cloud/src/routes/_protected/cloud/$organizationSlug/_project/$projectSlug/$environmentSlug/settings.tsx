import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { Separator } from "#/components/ui/separator";
import {
  environmentBySlugQueryOptions,
  projectBySlugQueryOptions,
} from "#/modules/environment-design/workspace-queries";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { Route as EnvironmentLayoutRoute } from "./route";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, projectSlug, environmentSlug } = Route.useParams();
  const { environmentId } = EnvironmentLayoutRoute.useLoaderData();
  const navigate = useNavigate();
  const project = useSuspenseQuery(
    projectBySlugQueryOptions(organizationSlug, projectSlug),
  );
  const environment = useSuspenseQuery(
    environmentBySlugQueryOptions(
      organizationSlug,
      projectSlug,
      environmentSlug,
    ),
  );

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
          confirmPhrase={environment.data.name}
          title="Tear down this environment"
          description="Destroys this environment’s services and volumes, then drops its Cloud rows. This cannot be undone."
          actionLabel="Tear down environment"
          headingId="environment-teardown-heading"
          onCompleted={leaveDeletedTree}
        />
        <Separator />
        <TeardownDangerSection
          organizationSlug={organizationSlug}
          scope="project"
          projectSlug={projectSlug}
          confirmPhrase={project.data.name}
          title="Tear down this project"
          description="Unions Data Loss across every environment in this project, then destroys them through Inngest. This cannot be undone."
          actionLabel="Tear down project"
          headingId="project-teardown-heading"
          showHeading={false}
          onCompleted={leaveDeletedTree}
        />
      </div>
    </DashboardPage>
  );
}
