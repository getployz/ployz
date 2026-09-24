import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { requireWorkspace } from "#/collections/route-data";
import { Route as EnvironmentOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/index";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/",
)({
  loader: async ({ params, context }) => {
    const projects = await requireWorkspace(context, params.organizationSlug);
    const environment = projects.find((project) => project.slug === params.projectSlug)?.resolvedEnvironment;
    if (!environment) throw notFound();

    throw redirect({
      to: EnvironmentOverviewRoute.to,
      params: {
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug: environment.namespace,
      },
      replace: true,
    });
  },
});
