import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import { getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { PendingEnrollmentResetSection } from "./PendingEnrollmentResetSection";

export function EnrollmentSection({ organizationSlug }: { organizationSlug: string }) {
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
