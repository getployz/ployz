import { createFileRoute } from "@tanstack/react-router";
import { EnvironmentPlaceholder } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/-components/EnvironmentPlaceholder";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/logs",
)({
  component: RouteComponent,
});

function RouteComponent() {
  return (
    <EnvironmentPlaceholder
      title="No logs yet"
      description="Deploy a service to start seeing logs here."
    />
  );
}
