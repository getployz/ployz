import { Suspense } from "react";
import { createFileRoute, Outlet } from "@tanstack/react-router";
import { prefetchStoreEnvironment, requireStoreEnvironment } from "#/collections/route-data";
import { LeaveWhenClosed } from "./-components/sync/branch-close";
import { branchQuery, configsQuery, diffQuery, domainsQuery, environmentSettingsQuery, namespaceQuery, servicesQuery, volumesQuery } from "#/modules/config-store/store-view.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    // The route names the Project and the Environment as the Config Store does.
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    const environment = await requireStoreEnvironment(context, params);
    await prefetchStoreEnvironment(context, params.organizationSlug, store,
      environmentSettingsQuery(store), servicesQuery(store), diffQuery(store), volumesQuery(store), configsQuery(store), namespaceQuery(store), domainsQuery(store),
      // A Branch's Live Nodes and Sync button; refused elsewhere.
      branchQuery(store));
    return { environmentId: environment.id, store };
  },
  component: EnvironmentLayout,
});

function EnvironmentLayout() {
  return (
    <>
      <Suspense fallback={null}><LeaveWhenClosed /></Suspense>
      <Outlet />
    </>
  );
}
