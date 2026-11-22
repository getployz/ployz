import type { OrganizationEnrollmentRow } from "./enrollment";
import { defineOrgCollection } from "#/collections/change-log/declare";

/** Whether the Organization's Cluster has its first Server: the pairing row's derived status. */
export const organizationEnrollment = defineOrgCollection({
  name: "organization_enrollment",
  sources: ["organization_pairing"],
  key: (row: OrganizationEnrollmentRow) => row.id,
});
