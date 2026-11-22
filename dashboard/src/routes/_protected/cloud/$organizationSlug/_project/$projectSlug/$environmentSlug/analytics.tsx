import { createFileRoute, useLoaderData } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote } from "#/collections/route-data";
import { requireStoreEnvironment } from "#/modules/config-store/store.route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { analyticsViewQueryOptions, apiViewQueryOptions, errorsViewQueryOptions } from "#/modules/web-analytics/ui/analytics.queries";
import { analyticsSearchSchema, toUrlSearch, withDefaults } from "#/modules/web-analytics/ui/analytics-search";
import { AnalyticsPage } from "#/modules/web-analytics/ui/analytics-view";
import { ENVIRONMENT_ROUTE_FROM } from "#/lib/environment-routes";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/analytics")({
  staticData: { section: "analytics" },
  validateSearch: Schema.toStandardSchemaV1(analyticsSearchSchema),
  loaderDeps: ({ search }) => withDefaults(search),
  loader: async ({ params, context, deps }) => {
    const environment = await requireStoreEnvironment(context, params);
    const scope = { organizationSlug: params.organizationSlug, environmentId: environment.id };
    const read = { window: deps.window, service: deps.service };
    await prefetchRemote(context, deps.view === "api" ? apiViewQueryOptions(scope, read)
      : deps.view === "errors" ? errorsViewQueryOptions(scope, read)
      : analyticsViewQueryOptions(scope, deps.range, deps.page));
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const search = withDefaults(Route.useSearch());
  const navigate = Route.useNavigate();
  return (
    <DashboardPage width="content" title="Analytics">
      <AnalyticsPage scope={{ organizationSlug, environmentId }} search={search} onSearch={(next) => navigate({ search: toUrlSearch(next) })} />
    </DashboardPage>
  );
}
