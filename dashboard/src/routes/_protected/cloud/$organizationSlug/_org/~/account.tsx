import { createFileRoute } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { prefetchRemote } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { accountSectionOrder } from "#/components/dashboard-navigation-model";
import { accountSessionsRead, linkedAccountsRead } from "#/modules/account/account.collection";
import { notificationSettingsQueryOptions } from "#/modules/notifications/notifications.queries";
import { AccountSectionView, VariantSwitcher } from "#/routes/_protected/cloud/account/-components/account-sections";

// Prototype: A is one section at a time behind the rail; C, D and E are every section on one scrolling page, and
// differ only in how the rail reaches it.
const accountSearchSchema = Schema.Struct({
  variant: Schema.optional(Schema.Literals(["a", "c", "d", "e", "f", "g"]).pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("a" as const))),
  )),
  section: Schema.optional(Schema.Literals(accountSectionOrder).pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("profile" as const))),
  )),
});

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/account")({
  validateSearch: Schema.toStandardSchemaV1(accountSearchSchema),
  loader: ({ context }) => {
    const scope = { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
    return prefetchRemote(context, notificationSettingsQueryOptions(), accountSessionsRead(scope), linkedAccountsRead(scope));
  },
  component: AccountInShell,
});

function AccountInShell() {
  const { variant = "a", section = "profile" } = Route.useSearch();
  return (
    <DashboardPage width="content">
      {variant !== "a" ? (
        <div className="flex flex-col gap-6">
          {accountSectionOrder.map((key) => (
            <div key={key} id={key} className="scroll-mt-16 border-t pt-6 first:border-t-0 first:pt-0"><AccountSectionView section={key} /></div>
          ))}
        </div>
      ) : <AccountSectionView section={section} />}
      <VariantSwitcher current={variant} />
    </DashboardPage>
  );
}
