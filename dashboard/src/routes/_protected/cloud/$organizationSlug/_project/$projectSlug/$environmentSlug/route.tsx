import { Suspense } from "react";
import { createFileRoute, Outlet } from "@tanstack/react-router";
import { prefetchStoreViews, requireStoreEnvironment } from "#/collections/route-data";
import { LeaveWhenClosed } from "./-components/sync/branch-close";
import { branchQuery } from "#/modules/config-store/store-view.queries";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
)({
  loader: async ({ params, context }) => {
    // The route names the Project and the Environment as the Config Store does.
    const store = { project: params.projectSlug, environment: params.environmentSlug };
    const environment = await requireStoreEnvironment(context, params);
    // Every page of a Branch leaves when it closes; refused elsewhere. Each page's own loader reads what it shows.
    await prefetchStoreViews(context, params.organizationSlug, branchQuery(store));
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
