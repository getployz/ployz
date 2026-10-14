import { createFileRoute } from "@tanstack/react-router";
import { prefetchRemote } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { accountSessionsRead, linkedAccountsRead } from "#/modules/account/account.collection";
import { notificationSettingsQueryOptions } from "#/modules/notifications/notifications.queries";
import { AccountNotificationsSection, ProfileSection, SessionsSection } from "../-components/account-sections";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/account")({
  loader: ({ context }) => {
    const scope = { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    return prefetchRemote(context, notificationSettingsQueryOptions(), accountSessionsRead(scope), linkedAccountsRead(scope));
  },
  component: AccountPage,
});

/** The signed-in user's own settings, the same in every Organization they belong to: one page, no sections in the rail. */
function AccountPage() {
  return (
    <DashboardPage width="content">
      <div className="flex flex-col gap-6">
        <ProfileSection />
        <SessionsSection />
        <AccountNotificationsSection />
      </div>
    </DashboardPage>
  );
}
