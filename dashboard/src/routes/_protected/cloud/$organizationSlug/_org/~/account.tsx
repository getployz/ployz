import { createFileRoute } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { passkeysQueryOptions } from "#/modules/passkeys/passkeys.queries";
import { prefetchRemote } from "#/collections/route-data";
import { accountReads, AccountSections } from "../-components/account-sections";
import { PasskeysSection } from "../-components/passkeys-section";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/account")({
  loader: ({ context, params }) => {
    const scope = { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    return prefetchRemote(scope, ...accountReads(scope), passkeysQueryOptions(params.organizationSlug));
  },
  component: AccountPage,
});

/** The signed-in user's own settings, the same in every Organization they belong to: one page, no sections in the rail. */
function AccountPage() {
  return (
    <DashboardPage width="content" title="Account">
      <div className="flex flex-col gap-6">
        <AccountSections />
        <PasskeysSection />
      </div>
    </DashboardPage>
  );
}
