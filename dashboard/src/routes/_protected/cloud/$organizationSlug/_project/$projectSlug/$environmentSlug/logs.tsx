import { createFileRoute } from "@tanstack/react-router";
import { ContainerLogs } from "#/components/container-logs";
import { storeEnabled } from "#/modules/config-store/store.contract";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/logs",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, projectSlug, environmentSlug } = Route.useParams();
  // Over the Config Store an Environment is named within its Project.
  const selection = storeEnabled ? { organizationSlug, projectSlug, environmentSlug } : { organizationSlug, environmentSlug };
  return <div className="flex h-full min-h-0 flex-col p-4"><ContainerLogs selection={selection} /></div>;
}
