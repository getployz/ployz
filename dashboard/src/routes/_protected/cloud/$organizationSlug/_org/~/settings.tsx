import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { DashboardPage } from "#/components/dashboard-page";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/settings",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const navigate = useNavigate();

  return (
    <DashboardPage width="content">
      <h1 className="sr-only">Server Settings</h1>
      <EnrollmentSection organizationSlug={organizationSlug} />
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
