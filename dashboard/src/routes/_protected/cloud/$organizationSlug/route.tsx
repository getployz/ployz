import { syncOrganizationSlugServerFn } from "#/modules/environment-design/workspace-functions";
import {
  organizationStateQueryOptions,
  projectListQueryOptions,
} from "#/modules/environment-design/workspace-queries";
import { RuntimeProvider } from "#/providers/runtime-provider";
import { hasPublicErrorCode } from "#/lib/public-error";
import {
  createFileRoute,
  notFound,
  Outlet,
} from "@tanstack/react-router";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug")({
  loader: async ({ params, context }) => {
    const session = context.session;

    if (session.session.activeOrganizationSlug !== params.organizationSlug) {
      try {
        await syncOrganizationSlugServerFn({
          data: { organizationSlug: params.organizationSlug },
        });
      } catch (error) {
        if (hasPublicErrorCode(error, "NOT_FOUND")) {
          throw notFound();
        }
        throw error;
      }
    }

    const navigationReady = Promise.all([
      context.queryClient.prefetchQuery(
        organizationStateQueryOptions(params.organizationSlug),
      ),
      context.queryClient.prefetchQuery(
        projectListQueryOptions(params.organizationSlug),
      ),
    ]);

    return { navigationReady };
  },
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();

  return (
    <RuntimeProvider organizationSlug={params.organizationSlug}>
      <Outlet />
    </RuntimeProvider>
  );
}
