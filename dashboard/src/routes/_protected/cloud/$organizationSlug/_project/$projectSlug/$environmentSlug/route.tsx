import { environmentManager } from "@tanstack/react-query";
import { environmentResourcesOptions } from "#/modules/environment-design/environment-data";
import { useEffect } from "react";
import { DashboardShell } from "#/components/dashboard-shell";
import { loadWorkspaceEnvironment, rememberSelectedEnvironment } from "#/modules/environment-design/workspace-queries";
import {
  createFileRoute,
  Outlet,
  useParams,
} from "@tanstack/react-router";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    const environment = await loadWorkspaceEnvironment(params, { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id });

    const navigationReady = context.queryClient.ensureQueryData(environmentResourcesOptions(params, {
      environmentSlug: params.environmentSlug, queryClient: context.queryClient,
      sessionId: context.session.session.id, userId: context.session.user.id,
    }));
    if (environmentManager.isServer()) await navigationReady;
    return {
      navigationReady,
      environmentId: environment.id,
      organizationId: environment.organizationId,
    };
  },
  component: RouteComponent,
});

function RouteComponent() {
  const params = useParams({
    from: "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
  });
  const { queryClient, session } = Route.useRouteContext();
  useEffect(() => {
    void rememberSelectedEnvironment({ queryClient, sessionId: session.session.id, userId: session.user.id }, {
      organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, environmentSlug: params.environmentSlug,
    });
  }, [queryClient, session.session.id, session.user.id, params.organizationSlug, params.projectSlug, params.environmentSlug]);
  return <DashboardShell scope={{ kind: "environment", ...params }}><Outlet /></DashboardShell>;
}
