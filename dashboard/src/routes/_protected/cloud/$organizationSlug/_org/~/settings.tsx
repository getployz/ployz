import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Effect, Option, Schema } from "effect";
import { getClusterDomainCollection, getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { prefetchRemote } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { BuildsSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/builds-settings";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";
import { ClusterDomainSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection";
import { checkClusterDomainNowServerFn } from "#/modules/cluster-domain/cluster-domain.functions";

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
    await prefetchRemote(context, latestTeardownAttemptQueryOptions({ organizationSlug: params.organizationSlug, scope: "organization" }));
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { section = "general" } = Route.useSearch();
  const navigate = useNavigate();

  return (
    <DashboardPage width="content">
      <h1 className="text-xl font-semibold">Settings</h1>
      <Tabs
        className="gap-4"
        value={section}
        onValueChange={(value) => {
          if (Schema.is(settingsSectionSchema)(value)) {
            void navigate({ to: "/cloud/$organizationSlug/~/settings", params: { organizationSlug }, search: { section: value }, replace: true });
          }
        }}
      >
        <TabsList variant="line">
          <TabsTrigger value="general">General</TabsTrigger>
          <TabsTrigger value="builds">Builds</TabsTrigger>
        </TabsList>
        <TabsContent value="general" className="flex flex-col gap-8">
          <EnrollmentSection organizationSlug={organizationSlug} />
          <ClusterDomainSettings organizationSlug={organizationSlug} />
          <TeardownDangerSection
            organizationSlug={organizationSlug}
            scope="organization"
            confirmPhrase={organizationSlug}
            title="Tear down this organization"
            description="Deletes all projects and their stored data, removes servers from the cluster, disconnects the cluster from Ployz, and deletes this organization. This cannot be undone."
            actionLabel="Tear down organization"
            headingId="organization-teardown-heading"
            onCompleted={() => {
              void navigate({ to: "/cloud", replace: true });
            }}
          />
        </TabsContent>
        <TabsContent value="builds">
          <BuildsSettings organizationSlug={organizationSlug} />
        </TabsContent>
      </Tabs>
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
