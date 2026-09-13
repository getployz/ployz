import {
  preferredEnvironmentQueryOptions,
} from "#/modules/environment-design/workspace-queries";
import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { Route as EnvironmentOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/index";
import { hasPublicErrorCode } from "#/lib/public-error";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/",
)({
  loader: async ({ params, context }) => {
    const environment = await context.queryClient
      .ensureQueryData(
        preferredEnvironmentQueryOptions(
          params.organizationSlug,
          params.projectSlug,
        ),
      )
      .catch((cause: unknown) => {
        if (hasPublicErrorCode(cause, "NOT_FOUND")) throw notFound();
        throw cause;
      });

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
