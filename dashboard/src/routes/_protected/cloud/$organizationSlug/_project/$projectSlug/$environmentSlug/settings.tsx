import { createFileRoute } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { prefetchRemoteWithStoreViews } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { missingStorePrGrantsQueryOptions, prPlansQuery } from "#/modules/config-store/store-pull-requests";
import { StoreEnvironmentSettings, StoreProjectSettings } from "./-components/store-settings";

const settingsSection = Schema.Literals(["environment", "project"]);

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    scope: Schema.optional(settingsSection.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
  })),
  // The Environment's loader read the Project's Environments; the rest are the PR plans and their GitHub permissions.
  loader: ({ params, context }) => prefetchRemoteWithStoreViews(context, params.organizationSlug,
    [prPlansQuery(params.projectSlug)], missingStorePrGrantsQueryOptions(params.organizationSlug, params.projectSlug)),
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  // The rail, or on phones the strip under the top bar, picks the section.
  const { scope: section = "environment" } = Route.useSearch();
  return (
    <DashboardPage width="content">
      <div className="flex flex-col gap-8">
        {section === "environment" ? <StoreEnvironmentSettings {...params} /> : <StoreProjectSettings {...params} />}
      </div>
    </DashboardPage>
  );
}
