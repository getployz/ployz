import { createFileRoute, redirect } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import { AccountSections, prefetchAccount } from "../$organizationSlug/_org/-components/account-sections";

/**
 * Email footers and older links land here. Account opens inside the Organization the user is acting in, else their
 * first one. A user in none (their only one was deleted) still gets their account, without an Organization around it.
 */
export const Route = createFileRoute("/_protected/cloud/account/")({
  beforeLoad: async ({ context, cause }) => {
    if (cause === "preload") return;
    const slug = context.session.session.activeOrganizationSlug
      ?? (await context.queryClient.ensureQueryData(organizationStateQueryOptions()))
        .organizations.find((organization) => organization.access === "member")?.slug;
    if (slug) throw redirect({ to: "/cloud/$organizationSlug/~/account", params: { organizationSlug: slug }, replace: true });
  },
  loader: ({ context }) =>
    prefetchAccount({ queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id }),
  component: AccountWithoutOrganization,
});

function AccountWithoutOrganization() {
  return (
    <DashboardPage width="content">
      <h1 className="text-lg font-semibold">Account</h1>
      <AccountSections />
    </DashboardPage>
  );
}
