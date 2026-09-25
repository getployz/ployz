import { useEffect } from "react";
import { createFileRoute, Outlet, useMatch, useParams } from "@tanstack/react-router";
import { useOrganizationChanges } from "#/collections/org-changes.stream";
import { prefetchOrgStore, requireOrganization } from "#/collections/route-data";
import { DashboardShell } from "#/components/dashboard-shell";
import type { DashboardScope } from "#/components/dashboard-navigation-model";
import { rememberSelectedOrganization } from "#/modules/environment-design/workspace.queries";
import { RuntimeProvider } from "#/providers/runtime-provider";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug")({
  loader: async ({ params, context }) => {
    await requireOrganization(context, params.organizationSlug);
    await prefetchOrgStore(context, params.organizationSlug);
  },
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const { queryClient, session } = Route.useRouteContext();
  useOrganizationChanges(params.organizationSlug);
  useEffect(() => {
    void rememberSelectedOrganization(queryClient, params.organizationSlug, session.session.activeOrganizationSlug);
  }, [queryClient, params.organizationSlug, session.session.activeOrganizationSlug]);

  return (
    <RuntimeProvider organizationSlug={params.organizationSlug}>
      <OrganizationLayout />
    </RuntimeProvider>
  );
}

/** One shell for every organization page, so navigation never remounts or hides it. */
function OrganizationLayout() {
  const { organizationSlug } = Route.useParams();
  const { projectSlug, environmentSlug } = useParams({ strict: false });
  const creatingProject = useMatch({ from: "/_protected/cloud/$organizationSlug/_project/new", shouldThrow: false });
  // Project creation is a focused full-screen flow.
  if (creatingProject) return <Outlet />;
  const scope: DashboardScope = projectSlug && environmentSlug
    ? { kind: "environment", organizationSlug, projectSlug, environmentSlug }
    : { kind: "all", organizationSlug };
  return <DashboardShell scope={scope}><Outlet /></DashboardShell>;
}
