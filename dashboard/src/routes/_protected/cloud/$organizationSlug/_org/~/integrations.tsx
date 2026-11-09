import { createFileRoute } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { Integrations } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/integrations";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/integrations")({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  return (
    <DashboardPage width="content" title="Organization">
      <Integrations organizationSlug={organizationSlug} />
    </DashboardPage>
  );
}
