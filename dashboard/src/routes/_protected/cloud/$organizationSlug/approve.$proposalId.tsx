import { createFileRoute } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { ApproveDeploy } from "#/modules/deploy-approval/approve-deploy";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/approve/$proposalId")({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, proposalId } = Route.useParams();
  return (
    <DashboardPage width="content" title="Approve a deploy">
      <ApproveDeploy organizationSlug={organizationSlug} id={proposalId} />
    </DashboardPage>
  );
}
