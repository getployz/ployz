import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Effect, Option, Schema } from "effect";
import { getClusterDomainCollection, getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { prefetchRemote, prefetchStoreViews } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useDeletionNodes } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { BuildsSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/builds-settings";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";
import { ClusterDomainSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection";
import { checkClusterDomainNowServerFn } from "#/modules/cluster-domain/cluster-domain.functions";
import { storeEnabled } from "#/modules/config-store/store.contract";
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
  loader: async ({ params, context }) => {
    if (storeEnabled) return prefetchStoreViews(context, params.organizationSlug, projectsQuery());
    await prefetchRemote(context, latestTeardownAttemptQueryOptions({ organizationSlug: params.organizationSlug, scope: "organization" }));
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { section = "general" } = Route.useSearch();

  return (
    <DashboardPage width="content">
      {/* The top bar names the page; the rail, or on phones the strip under the top bar, picks the section. */}
      {section === "builds" ? <BuildsSettings organizationSlug={organizationSlug} /> : (
        <div className="flex flex-col gap-8">
          <EnrollmentSection organizationSlug={organizationSlug} />
          <ClusterDomainSettings organizationSlug={organizationSlug} />
          {storeEnabled ? <StoreOrganizationDanger organizationSlug={organizationSlug} /> : <LegacyOrganizationDanger organizationSlug={organizationSlug} />}
        </div>
      )}
    </DashboardPage>
  );
}

// TODO(#1275): goes with the dark gate.
function LegacyOrganizationDanger({ organizationSlug }: { organizationSlug: string }) {
  const navigate = useNavigate();
  const { projects } = useWorkspace(organizationSlug);
  const nodes = useDeletionNodes(organizationSlug);
  return (
    <TeardownDangerSection
      organizationSlug={organizationSlug}
      scope="organization"
      name={organizationSlug}
      place={organizationSlug}
      title="Delete this organization"
      description="Its projects and data go with it, and its servers are reset."
      actionLabel="Delete organization"
      items={[...projects.map((project) => ({ kind: "project" as const, name: project.name })), ...nodes]}
      headingId="organization-teardown-heading"
      onCompleted={() => {
        void navigate({ to: "/cloud", replace: true });
      }}
    />
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
