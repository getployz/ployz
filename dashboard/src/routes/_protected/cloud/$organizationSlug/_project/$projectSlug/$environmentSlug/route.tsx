import { createFileRoute } from "@tanstack/react-router";
import { prefetchStoreEnvironment, requireOrganization, requireStoreEnvironment } from "#/collections/route-data";
import { branchQuery, diffQuery, environmentSettingsQuery, namespaceQuery, saveQuery, servicesQuery, volumesQuery } from "#/modules/config-store/store-view.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    // The route names the Project and the Environment as the Config Store does.
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    const environment = await requireStoreEnvironment(context, params);
    // The organization route already required it, so this is its cached answer.
    const organization = await requireOrganization(context, params.organizationSlug);
    await prefetchStoreEnvironment(context, params.organizationSlug, store,
      environmentSettingsQuery(store), servicesQuery(store), diffQuery(store), volumesQuery(store), namespaceQuery(store),
      // A Branch's Live Nodes and its button's news; refused elsewhere.
      branchQuery(store), saveQuery(store));
    return { environmentId: environment.id, organizationId: organization.id, store };
  },
});
