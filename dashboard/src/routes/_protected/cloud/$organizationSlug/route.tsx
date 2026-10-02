import { useEffect } from "react";
import { createFileRoute, Outlet, useParams } from "@tanstack/react-router";
import { useOrganizationChanges } from "#/collections/org-changes.stream";
import { prefetchOrgStore, requireOrganization } from "#/collections/route-data";
import { DashboardShell } from "#/components/dashboard-shell";
import type { DashboardScope } from "#/components/dashboard-navigation-model";
import { rememberSelectedOrganization } from "#/modules/organization/organization-state.queries";
import { RuntimeProvider } from "#/providers/runtime-provider";
import { setPostHogOrganization } from "#/modules/analytics/posthog";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug")({
  loader: async ({ params, context }) => {
    const organization = await requireOrganization(context, params.organizationSlug);
    await prefetchOrgStore(context, params.organizationSlug);
    return { organizationId: organization.id, organizationName: organization.name };
  },
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const { queryClient, session } = Route.useRouteContext();
  const { organizationId, organizationName } = Route.useLoaderData();
  useOrganizationChanges(params.organizationSlug);
  useEffect(() => {
    setPostHogOrganization(organizationId, { name: organizationName, slug: params.organizationSlug });
  }, [organizationId, organizationName, params.organizationSlug]);
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
  const scope: DashboardScope = projectSlug && environmentSlug
    ? { kind: "environment", organizationSlug, projectSlug, environmentSlug }
    : { kind: "all", organizationSlug };
  return <DashboardShell scope={scope}><Outlet /></DashboardShell>;
}
