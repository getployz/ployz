import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { requireStoreProjects, requireWorkspace } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { Route as EnvironmentOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/index";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/",
)({
  loader: async ({ params, context }) => {
    // A project opens its Default Environment.
    const environmentSlug = storeEnabled
      ? (await requireStoreProjects(context, params.organizationSlug))
        .find((project) => project.name === params.projectSlug)?.default_environment
      : (await requireWorkspace(context, params.organizationSlug))
        .find((project) => project.slug === params.projectSlug)?.resolvedEnvironment?.namespace;
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
