import { createFileRoute } from "@tanstack/react-router";
import { prefetchStoreViews, requireEnvironment } from "#/collections/route-data";
import { environmentSettingsQuery } from "#/modules/config-store/store-view.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    const environment = await requireEnvironment(context, params);
    // TODO(#1267): Projects and Environments move to the Store; until then the Store names them like the dashboard.
    const store = { project: params.projectSlug, environment: environment.name };
    await prefetchStoreViews(context, params.organizationSlug, environmentSettingsQuery(store));
    return { environmentId: environment.id, organizationId: environment.organizationId, store };
  },
});
