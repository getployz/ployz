import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/",
)({
  component: RouteComponent,
});

function RouteComponent() {
  return null;
}
