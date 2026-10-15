import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/analytics")({
  component: RouteComponent,
});

function RouteComponent() {
  return <div className="flex h-full min-h-0 flex-col p-4" />;
}
