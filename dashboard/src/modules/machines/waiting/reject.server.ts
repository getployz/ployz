import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { cancelServerEnrollments } from "#/modules/machines/waiting/enrollment-run.server";
import { PAST_STORAGE } from "#/modules/machines/waiting/server-enrollment";
import { serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";

export const rejectWaitingServers = Effect.fn("WaitingServer.reject")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly ids: ReadonlyArray<string> },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:add");
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // Every writer of several waiting rows locks them in id order first, so two of them can't deadlock.
    // It writes only the rows it locked. Re-evaluating the filter could take a row a check-in just un-rejected, out of order.
    const rejected = yield* drizzle.select({ id: waitingServer.id }).from(waitingServer).where(and(
      eq(waitingServer.organizationId, organization.id),
      inArray(waitingServer.id, [...input.ids]),
      isNull(waitingServer.rejectedAt),
    )).orderBy(waitingServer.id).for("update");
    if (rejected.length > 0) {
      yield* drizzle.update(waitingServer).set({ rejectedAt: new Date() })
        .where(inArray(waitingServer.id, rejected.map((row) => row.id)));
      const open = yield* drizzle.select({ id: serverEnrollment.id, stage: serverEnrollment.stage }).from(serverEnrollment).where(and(
        inArray(serverEnrollment.waitingServerId, rejected.map((row) => row.id)),
        isNull(serverEnrollment.endedAt),
      )).orderBy(serverEnrollment.id).for("update");
      yield* cancelServerEnrollments(organization.id, open.filter((row) => !PAST_STORAGE.has(row.stage)).map((row) => row.id));
    }
    return { rejected: rejected.length };
  }));
});
