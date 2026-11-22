import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { organizationEnrollment } from "./enrollment.collection";
import { type OrganizationEnrollmentRow, pairingEnrollmentStatus } from "./enrollment";
import { organizationPairing } from "#/modules/runtime/tables";
import { defineProjection } from "#/collections/change-log/projection.server";
import { Database } from "#/server/database.server";

// The pairing row holds the encrypted pairing secret; expose only the derived status.
export const organizationEnrollmentProjection = defineProjection(organizationEnrollment, (scoped) => Effect.gen(function* () {
  const database = yield* Database;
  const pairings = yield* database.drizzle.select({
    id: organizationPairing.organizationId,
    founderMachineId: organizationPairing.founderMachineId,
  }).from(organizationPairing).where(scoped(organizationPairing));
  return pairings.map((row): OrganizationEnrollmentRow => ({ id: row.id, status: pairingEnrollmentStatus(row.founderMachineId) }));
}));
