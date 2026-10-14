import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { PloyzMark } from "#/components/icons/ployz-logo";
import { buttonVariants } from "#/components/ui/button-variants";
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
    <div className="flex min-h-dvh flex-col">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3 md:px-4">
        <Link to="/cloud" aria-label="Projects" title="Projects" className={buttonVariants({ variant: "ghost", size: "icon-lg" })}>
          <PloyzMark decorative className="size-7" />
        </Link>
        <h1 className="text-sm font-semibold">Account</h1>
      </header>
      <DashboardPage width="content">
        <AccountSections />
      </DashboardPage>
    </div>
  );
}
