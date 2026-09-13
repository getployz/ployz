import { createFileRoute } from "@tanstack/react-router";
import { ProjectRequiredState } from "#/components/project-required-state";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/logs",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();

  return (
    <ProjectRequiredState
      organizationSlug={organizationSlug}
      section="logs"
      title="Select a project to view logs"
      description="Logs belong to a specific project and environment."
    />
  );
}
