import { createFileRoute } from "@tanstack/react-router";
import { prefetchStoreEnvironment, requireEnvironment, requireOrganization, requireStoreEnvironment } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { diffQuery, environmentSettingsQuery, servicesQuery, volumesQuery } from "#/modules/config-store/store-view.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    // Over the Config Store the route names the Project and the Environment as the Store does.
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    if (!storeEnabled) {
      const environment = await requireEnvironment(context, params);
      return { environmentId: environment.id, organizationId: environment.organizationId, store };
    }
    const environment = await requireStoreEnvironment(context, params);
    // The organization route already required it, so this is its cached answer.
    const organization = await requireOrganization(context, params.organizationSlug);
    await prefetchStoreEnvironment(context, params.organizationSlug, store,
      environmentSettingsQuery(store), servicesQuery(store), diffQuery(store), volumesQuery(store));
    return { environmentId: environment.id, organizationId: organization.id, store };
  },
});
