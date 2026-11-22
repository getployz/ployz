import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { requireOrganization, requireStoreProjects } from "#/collections/route-data";
import { ENVIRONMENT_INDEX_ROUTE_TO } from "#/lib/environment-routes";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/",
)({
  // A typed or bookmarked Project URL; in-app links name the Environment. `beforeLoad` decides before the Organization's
  // loader reads for a page that never renders. Access comes first, so a removed member reads "no longer a member".
  // No in-app link may target this URL: on a client navigation this `beforeLoad` reads over the network.
  beforeLoad: async ({ params, context }) => {
    await requireOrganization(context, params.organizationSlug);
    const environmentSlug = (await requireStoreProjects(context, params.organizationSlug))
      .find((project) => project.name === params.projectSlug)?.default_environment;
    if (!environmentSlug) throw notFound();

    throw redirect({
      to: ENVIRONMENT_INDEX_ROUTE_TO,
      params: {
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug,
      },
      replace: true,
    });
  },
});
