import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { createFileRoute } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Effect, Option, Schema } from "effect";
import { getClusterDomainCollection, getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { prefetchStoreViews } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { BUILD_ORDER_QUERY, BuildsSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/builds-settings";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";
import { ClusterDomainSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection";
import { checkClusterDomainNowServerFn } from "#/modules/cluster-domain/cluster-domain.functions";
import { projectsQuery } from "#/modules/config-store/store-view.queries";
import { StoreOrganizationDanger } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/store-organization-danger";

const settingsSectionSchema = Schema.Literals(["general", "builds"]);

// Not `tab`: search keys share one type across routes, and the service panel's `tab` means something else.
const settingsSearchSchema = Schema.Struct({
  section: Schema.optional(settingsSectionSchema.pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("general" as const))),
  )),
});

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/settings",
)({
  validateSearch: Schema.toStandardSchemaV1(settingsSearchSchema),
  loader: ({ params, context }) => prefetchStoreViews(context, params.organizationSlug, projectsQuery(), BUILD_ORDER_QUERY),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { section = "general" } = Route.useSearch();

  return (
    <DashboardPage width="content">
      {/* The top bar names the page; the rail, or on phones the strip under the top bar, picks the section. */}
      {section === "builds" ? <BuildsSettings organizationSlug={organizationSlug} /> : (
        <div className="flex flex-col gap-6">
          <EnrollmentSection organizationSlug={organizationSlug} />
          <ClusterDomainSettings organizationSlug={organizationSlug} />
          <StoreOrganizationDanger organizationSlug={organizationSlug} />
        </div>
      )}
    </DashboardPage>
  );
}

function EnrollmentSection({ organizationSlug }: { organizationSlug: string }) {
  const enrollment = getOrganizationEnrollmentCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(enrollment);
  const status = organizationEnrollmentStatus(rows[0]);
  const resetPendingEnrollment = useServerFn(resetPendingOrganizationEnrollmentServerFn);
  return (
    <PendingEnrollmentResetSection
      status={status}
      onReset={({ confirmedFounderStoppedOrErased }) =>
        resetPendingEnrollment({
          data: { organizationSlug, confirmedFounderStoppedOrErased },
        }).then(() => undefined)
      }
      onCompleted={() => {
        void reconcileCollection(enrollment);
      }}
    />
  );
}

function ClusterDomainSettings({ organizationSlug }: { organizationSlug: string }) {
  const { data: rows } = useLiveSuspenseQuery(getClusterDomainCollection(organizationSlug, useCollectionScope()));
  const checkNow = useServerFn(checkClusterDomainNowServerFn);
  return (
    <ClusterDomainSection
      organizationSlug={organizationSlug}
      domain={rows[0] ?? null}
      onCheck={() => checkNow({ data: { organizationSlug } })}
    />
  );
}
