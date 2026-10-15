import { createFileRoute, useLoaderData } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote, requireStoreEnvironment } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { DEFAULT_RANGE } from "#/modules/web-analytics/ui/analytics-model";
import { analyticsViewQueryOptions } from "#/modules/web-analytics/ui/analytics.queries";
import { AnalyticsPage } from "#/modules/web-analytics/ui/analytics-view";
import { ENVIRONMENT_ROUTE_FROM } from "./-components/environment-route-paths";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/analytics")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    range: Schema.optional(Schema.String),
    page: Schema.optional(Schema.String),
  })),
  loaderDeps: ({ search }) => ({ range: search.range, page: search.page }),
  loader: async ({ params, context, deps }) => {
    const environment = await requireStoreEnvironment(context, params);
    await prefetchRemote(context, analyticsViewQueryOptions(
      { organizationSlug: params.organizationSlug, environmentId: environment.id }, deps.range ?? DEFAULT_RANGE, deps.page));
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { range = DEFAULT_RANGE, page } = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <DashboardPage width="content">
      <AnalyticsPage scope={{ organizationSlug, environmentId }} search={{ range, ...(page !== undefined && { page }) }}
        onSearch={(next) => navigate({ search: { ...(next.range !== DEFAULT_RANGE && { range: next.range }), ...(next.page !== undefined && { page: next.page }) } })} />
    </DashboardPage>
  );
}
