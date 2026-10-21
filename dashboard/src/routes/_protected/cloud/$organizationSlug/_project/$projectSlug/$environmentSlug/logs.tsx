import { createFileRoute } from "@tanstack/react-router";
import { ContainerLogs } from "#/components/container-logs";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/logs",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, projectSlug, environmentSlug } = Route.useParams();
  return (
    <div className="flex h-full min-h-0 flex-col p-4">
      <h1 className="sr-only">Logs</h1>
      <ContainerLogs selection={{ organizationSlug, projectSlug, environmentSlug }} />
    </div>
  );
}
