import { createFileRoute, Outlet, useParams } from "@tanstack/react-router";
import { DashboardShell } from "#/components/dashboard-shell";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org")({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = useParams({ from: "/_protected/cloud/$organizationSlug" });
  return <DashboardShell scope={{ kind: "all", organizationSlug }}><Outlet /></DashboardShell>;
}
