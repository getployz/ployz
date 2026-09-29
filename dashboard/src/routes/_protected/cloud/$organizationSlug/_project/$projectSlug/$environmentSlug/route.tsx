import { createFileRoute } from "@tanstack/react-router";
import { prefetchStoreEnvironment, requireEnvironment } from "#/collections/route-data";
import { branchQuery, diffQuery, environmentSettingsQuery, saveQuery, servicesQuery, volumesQuery } from "#/modules/config-store/store-view.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    const environment = await requireEnvironment(context, params);
    // TODO(#1267): Projects and Environments move to the Store; until then the Store names them like the dashboard.
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    await prefetchStoreEnvironment(context, params.organizationSlug, store,
      environmentSettingsQuery(store), servicesQuery(store), diffQuery(store), volumesQuery(store),
      // A Branch's Live Nodes and its button's news; refused elsewhere.
      branchQuery(store), saveQuery(store));
    return { environmentId: environment.id, organizationId: environment.organizationId, store };
  },
});
