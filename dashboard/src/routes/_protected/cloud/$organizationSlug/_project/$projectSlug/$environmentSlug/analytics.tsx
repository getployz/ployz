import { createFileRoute, useLoaderData } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchRemote, requireStoreEnvironment } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { DEFAULT_RANGE, DEFAULT_WINDOW, isAnalyticsView, isRequestWindow } from "#/modules/web-analytics/ui/analytics-model";
import { analyticsViewQueryOptions, apiViewQueryOptions, errorsViewQueryOptions } from "#/modules/web-analytics/ui/analytics.queries";
import { type AnalyticsSearch, AnalyticsPage } from "#/modules/web-analytics/ui/analytics-view";
import { ENVIRONMENT_ROUTE_FROM } from "./-components/environment-route-paths";

type RawSearch = { view?: string; range?: string; page?: string; window?: string; service?: string };

function parseSearch(raw: RawSearch): AnalyticsSearch {
  return {
    view: raw.view !== undefined && isAnalyticsView(raw.view) ? raw.view : "web",
    range: raw.range ?? DEFAULT_RANGE,
    window: raw.window !== undefined && isRequestWindow(raw.window) ? raw.window : DEFAULT_WINDOW,
    ...(raw.page !== undefined && { page: raw.page }),
    ...(raw.service !== undefined && raw.service !== "" && { service: raw.service }),
  };
}

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/analytics")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    view: Schema.optional(Schema.String),
    range: Schema.optional(Schema.String),
    page: Schema.optional(Schema.String),
    window: Schema.optional(Schema.String),
    service: Schema.optional(Schema.String),
  })),
  loaderDeps: ({ search }) => parseSearch(search),
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
  const search = parseSearch(Route.useSearch());
  const navigate = Route.useNavigate();
  return (
    <DashboardPage width="content">
      <AnalyticsPage scope={{ organizationSlug, environmentId }} search={search}
        onSearch={(next) => navigate({
          search: {
            ...(next.view !== "web" && { view: next.view }),
            ...(next.range !== DEFAULT_RANGE && { range: next.range }),
            ...(next.page !== undefined && { page: next.page }),
            ...(next.window !== DEFAULT_WINDOW && { window: next.window }),
            ...(next.service !== undefined && { service: next.service }),
          },
        })} />
    </DashboardPage>
  );
}
