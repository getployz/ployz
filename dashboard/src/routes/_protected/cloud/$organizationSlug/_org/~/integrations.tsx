import { createFileRoute } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { useMemberGate } from "#/modules/organization/members.queries";
import { Integrations } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/integrations";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/integrations")({
  staticData: { section: "integrations" },
  component: RouteComponent,
});


function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const gate = useMemberGate(organizationSlug, "servers:operate");
  return (
    <DashboardPage width="content" title="Organization">
      <Integrations organizationSlug={organizationSlug} editable={gate.allowed} />
    </DashboardPage>
  );
}
