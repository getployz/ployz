import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { prefetchRemote } from "#/collections/route-data";
import { accountSectionLabels, accountSectionOrder } from "#/components/dashboard-navigation-model";
import { PloyzMark } from "#/components/icons/ployz-logo";
import { buttonVariants } from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";
import { accountSessionsRead, linkedAccountsRead } from "#/modules/account/account.collection";
import { notificationSettingsQueryOptions } from "#/modules/notifications/notifications.queries";
import { AccountSectionView, VariantSwitcher } from "./-components/account-sections";

// Prototype: no variant is B, this page. A and C live in the active Organization's shell; without one they fall back here.
const accountSearchSchema = Schema.Struct({
  variant: Schema.optional(Schema.Literals(["a", "b", "c", "d", "e"]).pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("b" as const))),
  )),
  section: Schema.optional(Schema.Literals(accountSectionOrder).pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("profile" as const))),
  )),
});

export const Route = createFileRoute("/_protected/cloud/account/")({
  validateSearch: Schema.toStandardSchemaV1(accountSearchSchema),
  beforeLoad: ({ context, search, cause }) => {
    const slug = context.session.session.activeOrganizationSlug;
    if (cause === "preload" || !slug || search.variant === undefined || search.variant === "b") return;
    throw redirect({ to: "/cloud/$organizationSlug/~/account", params: { organizationSlug: slug }, search: { variant: search.variant, section: search.section } });
  },
  loader: ({ context }) => {
    const scope = { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    return prefetchRemote(context, notificationSettingsQueryOptions(), accountSessionsRead(scope), linkedAccountsRead(scope));
  },
  component: AccountPage,
});

/** The signed-in user's own settings, the same in every Organization they belong to. */
function AccountPage() {
  const { section = "profile" } = Route.useSearch();
  return (
    <div className="flex min-h-dvh flex-col">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3 md:px-4">
        <Link to="/cloud" aria-label="Projects" title="Projects" className={buttonVariants({ variant: "ghost", size: "icon-lg" })}>
          <PloyzMark decorative className="size-7" />
        </Link>
        <h1 className="text-sm font-semibold">Account</h1>
      </header>
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-6 px-4 py-6 md:flex-row md:gap-10 md:px-6 md:py-10">
        <nav aria-label="Account sections" className="-mx-4 flex shrink-0 gap-1 overflow-x-auto border-b px-4 pb-2 md:mx-0 md:w-44 md:flex-col md:border-b-0 md:px-0 md:pb-0">
          {accountSectionOrder.map((key) => (
            <Link key={key} to="/cloud/account" search={{ variant: "b", section: key }}
              aria-current={key === section ? "page" : undefined}
              className={cn(
                "shrink-0 rounded-md px-2.5 py-1.5 text-sm",
                key === section ? "bg-accent font-medium text-foreground" : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
              )}>
              {accountSectionLabels[key]}
            </Link>
          ))}
        </nav>
        <main className="min-w-0 flex-1"><AccountSectionView section={section} /></main>
      </div>
      <VariantSwitcher current="b" />
    </div>
  );
}
