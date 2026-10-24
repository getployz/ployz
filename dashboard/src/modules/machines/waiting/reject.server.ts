import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { cancelServerEnrollments } from "#/modules/machines/waiting/enrollment-run.server";
import { joinCommitted } from "#/modules/machines/waiting/server-enrollment";
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
    const rejected = yield* drizzle.select({
      id: waitingServer.id,
      reportedAttemptId: waitingServer.reportedAttemptId,
      reportedStage: waitingServer.reportedStage,
    }).from(waitingServer).where(and(
      eq(waitingServer.organizationId, organization.id),
      inArray(waitingServer.id, [...input.ids]),
      isNull(waitingServer.rejectedAt),
    )).orderBy(waitingServer.id).for("update");
    // The rejected hosts whose open attempt runs on, because Cloud may have sent its join.
    let spared: ReadonlyArray<string> = [];
    if (rejected.length > 0) {
      yield* drizzle.update(waitingServer).set({ rejectedAt: new Date() })
        .where(inArray(waitingServer.id, rejected.map((row) => row.id)));
      const open = yield* drizzle.select({
        id: serverEnrollment.id,
        stage: serverEnrollment.stage,
        waitingServerId: serverEnrollment.waitingServerId,
      }).from(serverEnrollment).where(and(
        inArray(serverEnrollment.waitingServerId, rejected.map((row) => row.id)),
        isNull(serverEnrollment.endedAt),
      )).orderBy(serverEnrollment.id).for("update");
      const reports = new Map(rejected.map((row) => [row.id, row]));
      const cancelled = open.filter((attempt) => {
        const row = reports.get(attempt.waitingServerId);
        return row === undefined || !joinCommitted(attempt, row);
      });
      const kept = new Set(yield* cancelServerEnrollments(organization.id, cancelled.map((attempt) => attempt.id)));
      spared = open.filter((attempt) => !cancelled.includes(attempt) || kept.has(attempt.id)).map((attempt) => attempt.waitingServerId);
    }
    return { rejected: rejected.length, spared };
  }));
});
