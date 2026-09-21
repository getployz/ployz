import { environmentManager } from "@tanstack/react-query";
import { useEffect } from "react";
import { rememberSelectedOrganization } from "#/modules/environment-design/workspace-queries";
import {
  organizationStateQueryOptions,
  preloadWorkspace,
} from "#/modules/environment-design/workspace-queries";
import { RuntimeProvider } from "#/providers/runtime-provider";
import {
  createFileRoute,
  notFound,
  Outlet,
} from "@tanstack/react-router";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug")({
  loader: async ({ params, context }) => {
    const organization = await context.queryClient.ensureQueryData(
      organizationStateQueryOptions(params.organizationSlug),
    );
    if (organization.activeOrganization?.slug !== params.organizationSlug) throw notFound();

    const navigationReady = preloadWorkspace(params.organizationSlug, {
      queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id,
    });

    if (environmentManager.isServer()) await navigationReady;
    return { navigationReady: navigationReady.then(() => undefined) };
  },
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const { queryClient, session } = Route.useRouteContext();
  useEffect(() => {
    void rememberSelectedOrganization(queryClient, params.organizationSlug, session.session.activeOrganizationSlug);
  }, [queryClient, params.organizationSlug, session.session.activeOrganizationSlug]);

  return (
    <RuntimeProvider organizationSlug={params.organizationSlug}>
      <Outlet />
    </RuntimeProvider>
  );
}
