import { createFileRoute } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { AccountSections, prefetchAccount } from "../-components/account-sections";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/account")({
  loader: ({ context }) =>
    prefetchAccount({ queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id }),
  component: AccountPage,
});

/** The signed-in user's own settings, the same in every Organization they belong to: one page, no sections in the rail. */
function AccountPage() {
  return (
    <DashboardPage width="content">
      <AccountSections />
    </DashboardPage>
  );
}
