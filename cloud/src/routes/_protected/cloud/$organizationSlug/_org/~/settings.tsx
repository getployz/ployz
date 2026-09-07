import {
  Await,
  createFileRoute,
  useNavigate,
  useRouter,
} from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { DashboardPage } from "#/components/dashboard-page";
import { Skeleton } from "#/components/ui/skeleton";
import {
  loadOrganizationEnrollmentStatusServerFn,
  resetPendingOrganizationEnrollmentServerFn,
} from "#/modules/machines/enrollment.functions";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/settings",
)({
  loader: ({ params }) => ({
    enrollmentStatus: loadOrganizationEnrollmentStatusServerFn({
      data: { organizationSlug: params.organizationSlug },
    }),
  }),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { enrollmentStatus } = Route.useLoaderData();
  const navigate = useNavigate();
  const router = useRouter();
  const resetPendingEnrollment = useServerFn(
    resetPendingOrganizationEnrollmentServerFn,
  );

  return (
    <DashboardPage width="content">
      <h1 className="sr-only">Server Settings</h1>
      <Await promise={enrollmentStatus} fallback={<Skeleton className="h-24 w-full" />}>
        {(status) => (
          <PendingEnrollmentResetSection
            status={status}
            onReset={({ confirmedFounderStoppedOrErased }) =>
              resetPendingEnrollment({
                data: {
                  organizationSlug,
                  confirmedFounderStoppedOrErased,
                },
              }).then(() => undefined)
            }
            onCompleted={() => {
              void router.invalidate();
            }}
          />
        )}
      </Await>
      <TeardownDangerSection
        organizationSlug={organizationSlug}
        scope="organization"
        confirmPhrase={organizationSlug}
        title="Tear down this organization"
        description="Destroys every project’s Data Loss, removes machines, revokes pairing, then drops Cloud org rows. This cannot be undone."
        actionLabel="Tear down organization"
        headingId="organization-teardown-heading"
        onCompleted={() => {
          void navigate({ to: "/cloud", replace: true });
        }}
      />
    </DashboardPage>
  );
}
