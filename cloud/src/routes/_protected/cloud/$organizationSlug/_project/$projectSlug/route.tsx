import { createFileRoute, Outlet } from "@tanstack/react-router";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug",
)({
  component: RouteComponent,
});

function RouteComponent() {
  return <Outlet />;
}
