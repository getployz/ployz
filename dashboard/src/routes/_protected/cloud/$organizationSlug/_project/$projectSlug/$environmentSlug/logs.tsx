import { createFileRoute } from "@tanstack/react-router";
import { ContainerLogs } from "#/components/container-logs";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/logs",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, environmentSlug } = Route.useParams();
  return <div className="p-4"><ContainerLogs selection={{ organizationSlug, environmentSlug }} /></div>;
}
