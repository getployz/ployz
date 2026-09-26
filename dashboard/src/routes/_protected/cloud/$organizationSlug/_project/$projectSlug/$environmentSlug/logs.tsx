import { createFileRoute } from "@tanstack/react-router";
import { ContainerLogs } from "#/components/container-logs";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/logs",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, environmentSlug } = Route.useParams();
  return <div className="flex h-full min-h-0 flex-col p-4"><ContainerLogs selection={{ organizationSlug, environmentSlug }} /></div>;
}
