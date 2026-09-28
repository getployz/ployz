import { createFileRoute } from "@tanstack/react-router";
import { requireEnvironment } from "#/collections/route-data";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    const environment = await requireEnvironment(context, params);
    return { environmentId: environment.id, organizationId: environment.organizationId };
  },
});
