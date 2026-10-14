import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { prefetchRemote } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { PloyzMark } from "#/components/icons/ployz-logo";
import { buttonVariants } from "#/components/ui/button-variants";
import { notificationSettingsQueryOptions, useSetNotificationChannel } from "#/modules/notifications/notifications.queries";
import { NotificationsSection } from "./-components/notifications-section";

export const Route = createFileRoute("/_protected/cloud/account/")({
  loader: ({ context }) => prefetchRemote(context, notificationSettingsQueryOptions()),
  component: AccountPage,
});

/** The signed-in user's own settings, the same in every Organization they belong to. */
function AccountPage() {
  const { data: settings } = useSuspenseQuery(notificationSettingsQueryOptions());
  const setChannel = useSetNotificationChannel();
  return (
    <div className="flex min-h-dvh flex-col">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3 md:px-4">
        <Link to="/cloud" aria-label="Projects" title="Projects" className={buttonVariants({ variant: "ghost", size: "icon-lg" })}>
          <PloyzMark decorative className="size-7" />
        </Link>
        <h1 className="text-sm font-semibold">Account</h1>
      </header>
      <DashboardPage width="content">
        <NotificationsSection settings={settings} onChange={(change) => setChannel.mutate(change)} />
      </DashboardPage>
    </div>
  );
}
