import {
  preloadWorkspace, readWorkspace,
} from "#/modules/environment-design/workspace-queries";
import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { Route as EnvironmentOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/index";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/",
)({
  loader: async ({ params, context }) => {
    const collections = await preloadWorkspace(params.organizationSlug, { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id });
    const environment = readWorkspace(collections).find((project) => project.slug === params.projectSlug)?.resolvedEnvironment;
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
