import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { requireStoreProjects } from "#/collections/route-data";
import { Route as EnvironmentOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/index";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/",
)({
  loader: async ({ params, context }) => {
    // A project opens its Default Environment.
    const environmentSlug = (await requireStoreProjects(context, params.organizationSlug))
      .find((project) => project.name === params.projectSlug)?.default_environment;
    if (!environmentSlug) throw notFound();

    throw redirect({
      to: EnvironmentOverviewRoute.to,
      params: {
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug,
      },
      replace: true,
    });
  },
});
