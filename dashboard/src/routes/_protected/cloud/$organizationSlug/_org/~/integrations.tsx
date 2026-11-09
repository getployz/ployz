import { createFileRoute, getRouteApi } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { useRoleGate } from "#/modules/organization/members.queries";
import { Integrations } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/integrations";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/integrations")({
  component: RouteComponent,
});

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { access } = organizationRoute.useLoaderData();
  const gate = useRoleGate(organizationSlug, "servers:operate");
  return (
    <DashboardPage width="content" title="Organization">
      <Integrations organizationSlug={organizationSlug} editable={access !== "support" && gate.allowed} />
    </DashboardPage>
  );
}
