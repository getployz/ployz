import { useEffect } from "react";
import { createFileRoute, Outlet } from "@tanstack/react-router";
import { requireEnvironment } from "#/collections/route-data";
import { rememberSelectedEnvironment } from "#/modules/environment-design/workspace.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    const environment = await requireEnvironment(context, params);
    return { environmentId: environment.id, organizationId: environment.organizationId };
  },
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const { queryClient, session } = Route.useRouteContext();
  useEffect(() => {
    void rememberSelectedEnvironment({ queryClient, sessionId: session.session.id, userId: session.user.id }, {
      organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, environmentSlug: params.environmentSlug,
    });
  }, [queryClient, session.session.id, session.user.id, params.organizationSlug, params.projectSlug, params.environmentSlug]);
  return <Outlet />;
}
