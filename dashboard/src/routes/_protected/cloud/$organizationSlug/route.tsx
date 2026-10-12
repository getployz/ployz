import { useEffect } from "react";
import { createFileRoute, Outlet, retainSearchParams, useNavigate, useParams } from "@tanstack/react-router";
import { Schema } from "effect";
import { useOrganizationChanges } from "#/collections/org-changes.stream";
import { prefetchOrgStore, requireOrganization } from "#/collections/route-data";
import { DashboardShell } from "#/components/dashboard-shell";
import type { DashboardScope } from "#/components/dashboard-navigation-model";
import { rememberSelectedOrganization } from "#/modules/organization/organization-state.queries";
import { RuntimeProvider } from "#/providers/runtime-provider";
import { setPostHogOrganization } from "#/modules/analytics/posthog";

/** `chat` names the open agent thread; it rides along every Organization page so the sidebar stays put. */
const OrganizationSearch = Schema.Struct({ chat: Schema.optional(Schema.String) });

export const Route = createFileRoute("/_protected/cloud/$organizationSlug")({
  validateSearch: Schema.toStandardSchemaV1(OrganizationSearch),
  search: { middlewares: [retainSearchParams(["chat"])] },
  loader: async ({ params, context }) => {
    const organization = await requireOrganization(context, params.organizationSlug);
    await prefetchOrgStore(context, params.organizationSlug);
    return { organizationId: organization.id, organizationName: organization.name, access: organization.access };
  },
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const { queryClient, session } = Route.useRouteContext();
  const { organizationId, organizationName, access } = Route.useLoaderData();
  useOrganizationChanges(params.organizationSlug);
  useEffect(() => {
    // A support visit isn't the customer's usage.
    if (access === "member") setPostHogOrganization(organizationId, { name: organizationName, slug: params.organizationSlug });
  }, [access, organizationId, organizationName, params.organizationSlug]);
  useEffect(() => {
    void rememberSelectedOrganization(queryClient, params.organizationSlug, session.session.activeOrganizationSlug);
  }, [queryClient, params.organizationSlug, session.session.activeOrganizationSlug]);

  return (
    <RuntimeProvider organizationSlug={params.organizationSlug}>
      <OrganizationLayout />
    </RuntimeProvider>
  );
}

/** One shell for every organization page, so navigation never remounts or hides it. */
function OrganizationLayout() {
  const { organizationSlug } = Route.useParams();
  const { projectSlug, environmentSlug } = useParams({ strict: false });
  const { chat } = Route.useSearch();
  const { organizationName, access } = Route.useLoaderData();
  const navigate = useNavigate();
  const scope: DashboardScope = projectSlug && environmentSlug
    ? { kind: "environment", organizationSlug, projectSlug, environmentSlug }
    : { kind: "all", organizationSlug };
  return (
    <DashboardShell scope={scope} chat={chat}
      banner={access === "support" ? (
        <div className="shrink-0 border-b bg-muted px-4 py-1.5 text-sm text-muted-foreground">Viewing {organizationName} as Ployz support.</div>
      ) : null}
      onChat={(thread) => void navigate({ to: ".", search: (prev) => ({ ...prev, chat: thread }), replace: true })}>
      <Outlet />
    </DashboardShell>
  );
}
