import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { deleteEnrollmentTokens } from "#/modules/machines/pairing-removal.server";
import { machineEnrollmentToken, machineRemoveAttempt, serverAccess } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";

/**
 * Hands every machines row that names `fromUserIds` to `toUserId`, for an Organization whose agent's user goes: the
 * caller deletes those users next, and the foreign keys would otherwise stop it. Only server access is scoped to the
 * Organization; the other tables name the user alone.
 */
export const reassignUserMachines = Effect.fn("Machines.reassignUserMachines")(function* (
  organizationId: string,
  fromUserIds: string[],
  toUserId: string,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(serverAccess).set({ userId: toUserId })
    .where(and(eq(serverAccess.organizationId, organizationId), inArray(serverAccess.userId, fromUserIds)));
  yield* drizzle.update(machineRemoveAttempt).set({ requestedByUserId: toUserId })
    .where(inArray(machineRemoveAttempt.requestedByUserId, fromUserIds));
  yield* drizzle.update(machineEnrollmentToken).set({ createdByUserId: toUserId })
    .where(inArray(machineEnrollmentToken.createdByUserId, fromUserIds));
});

/**
 * An Organization's enrollment rows, in the row removal's transaction after `lockOrganizationEnrollment` and the
 * Organization's own row.
 */
export const removeOrganizationMachines = Effect.fn("Machines.removeOrganizationMachines")(function* (organizationId: string) {
  yield* deleteEnrollmentTokens(organizationId);
});
