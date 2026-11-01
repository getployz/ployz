import "@tanstack/react-start/server-only";
import { and, eq, exists, inArray, isNotNull, isNull, lt, notExists, or, sql, type SQL } from "drizzle-orm";
import type { AnyPgColumn } from "drizzle-orm/pg-core";
import { Effect } from "effect";
import type { RunEnd } from "#/modules/inngest/attempt-lifecycle";
import { createServerEnrollmentCancelEvent } from "#/modules/inngest/events";
import type { MachineId } from "#/db/tables";
import { decryptPairingSecret } from "#/modules/machines/connections.server";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { enrollmentAllocation, machineEnrollmentToken } from "#/modules/machines/tables";
import { committedAfter, ENROLLMENT_STAGES, type EnrollmentFailure, type EnrollmentStage } from "#/modules/machines/waiting/server-enrollment";
import { fleetToken, fleetTokenJoin, serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";

export const CHECKIN_SILENCE_MS = 5 * 60_000;
export const ENROLLMENT_STALE_MS = 9 * 60 * 60_000;
export const JOINED_WAITING_KEEP_MS = 7 * 24 * 60 * 60_000;

export type EnrollmentRequest = { readonly attemptId: string; readonly organizationId: string };

const open = isNull(serverEnrollment.endedAt);
const ownedBy = (runId: string) => and(eq(serverEnrollment.inngestRunId, runId), open);

export const bindServerEnrollment = Effect.fn("ServerEnrollment.bind")(function* (request: EnrollmentRequest, runId: string) {
  const { drizzle } = yield* Database;
  const [bound] = yield* drizzle.update(serverEnrollment).set({ inngestRunId: runId }).where(and(
    eq(serverEnrollment.id, request.attemptId),
    eq(serverEnrollment.organizationId, request.organizationId),
    open,
    or(isNull(serverEnrollment.inngestRunId), eq(serverEnrollment.inngestRunId, runId)),
  )).returning({ role: serverEnrollment.role, state: serverEnrollment.state });
  if (bound !== undefined) return { kind: "bound", role: bound.role, state: bound.state } as const;
  const [row] = yield* drizzle.select({ state: serverEnrollment.state }).from(serverEnrollment).where(and(
    eq(serverEnrollment.id, request.attemptId),
    eq(serverEnrollment.organizationId, request.organizationId),
  ));
  return { kind: "settled", state: row?.state ?? null } as const;
});

export const organizationFounded = Effect.fn("ServerEnrollment.founded")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select({ founderMachineId: organizationPairing.founderMachineId })
    .from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  return pairing?.founderMachineId != null;
});

export const awaitingJoiners = Effect.fn("ServerEnrollment.awaitingJoiners")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ id: serverEnrollment.id }).from(serverEnrollment).where(and(
    eq(serverEnrollment.organizationId, organizationId),
    eq(serverEnrollment.state, "awaiting_founder"),
    open,
  ));
  return rows.map((row) => row.id);
});

export const releaseJoiner = Effect.fn("ServerEnrollment.release")(function* (attemptId: string, runId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.update(serverEnrollment).set({ state: "instructed" }).where(and(
    eq(serverEnrollment.id, attemptId),
    inArray(serverEnrollment.state, ["awaiting_founder", "instructed"]),
    ownedBy(runId),
  )).returning({ id: serverEnrollment.id });
  return rows.length === 1;
});

export const asStage = (stage: string | null): EnrollmentStage | null =>
  ENROLLMENT_STAGES.find((known) => known === stage) ?? null;

/** `next`, unless `column` already holds a committed stage further along, where `when` holds. Reports reach Cloud out of order. */
export const forwardTo = (column: AnyPgColumn, next: EnrollmentStage | null, when?: SQL) => {
  const later = committedAfter(next);
  return later.length === 0 ? next : sql<EnrollmentStage | null>`case when ${and(when, inArray(column, [...later]))} then ${column} else ${next} end`;
};

export const recordStage = Effect.fn("ServerEnrollment.recordStage")(function* (attemptId: string, runId: string, stage: string | null) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.update(serverEnrollment).set({ state: "running", stage: forwardTo(serverEnrollment.stage, asStage(stage)) }).where(and(
    eq(serverEnrollment.id, attemptId),
    ownedBy(runId),
  )).returning({ id: serverEnrollment.id });
  return rows.length === 1;
});

type Ending =
  | { readonly state: "joined"; readonly machineId: MachineId }
  | { readonly state: "failed"; readonly failure: EnrollmentFailure }
  | { readonly state: "cancelled" };

const endAndRevokeTokens = (where: ReturnType<typeof and>, ending: Ending) =>
  Effect.flatMap(Database, (database) => database.transaction(Effect.gen(function* () {
    if (ending.state === "joined") {
      const { drizzle } = yield* Database;
      // The ledger's foreign key takes this lock too; acquire it before attempts, as removal does.
      yield* drizzle.select({ id: fleetToken.id }).from(fleetToken)
        .innerJoin(serverEnrollment, eq(serverEnrollment.fleetTokenId, fleetToken.id))
        .where(where).orderBy(fleetToken.id).for("key share", { of: fleetToken });
    }
    return (yield* endAndRevokeTokensNow(where, ending)).length;
  })));

// Every bulk end locks its attempts in id order first, so two of them over the same attempts can't deadlock.
const endAndRevokeTokensNow = Effect.fn("ServerEnrollment.end")(function* (where: ReturnType<typeof and>, ending: Ending) {
  const { drizzle } = yield* Database;
  const locked = yield* drizzle.select({ id: serverEnrollment.id }).from(serverEnrollment)
    .where(where)
    .orderBy(serverEnrollment.id)
    .for("update");
  if (locked.length === 0) return [];
  const now = new Date();
  const ended = yield* drizzle.update(serverEnrollment).set({
    state: ending.state,
    machineId: ending.state === "joined" ? ending.machineId : null,
    failure: ending.state === "failed" ? ending.failure : null,
    encryptedToken: null,
    // The database's clock, as check-ins stamp theirs, so a listing can tell a report from before the join.
    endedAt: sql`now()`,
  }).where(and(inArray(serverEnrollment.id, locked.map((row) => row.id)), where)).returning({ id: serverEnrollment.id, enrollTokenId: serverEnrollment.enrollTokenId, fleetTokenId: serverEnrollment.fleetTokenId });
  if (ending.state === "joined" && ended.length > 0) {
    yield* drizzle.insert(fleetTokenJoin).values(ended.map((row) => ({ attemptId: row.id, fleetTokenId: row.fleetTokenId })));
  }
  if (ending.state !== "joined" && ended.length > 0) {
    yield* drizzle.update(machineEnrollmentToken).set({ expiresAt: sql`now()` }).where(and(
      inArray(machineEnrollmentToken.id, ended.map((row) => row.enrollTokenId)),
      isNull(machineEnrollmentToken.joinedMachineId),
    ));
  }
  return ended.map((row) => row.id);
});

export const endServerEnrollment = Effect.fn("ServerEnrollment.endRun")(function* (attemptId: string, runId: string, ending: Ending) {
  return (yield* endAndRevokeTokens(and(eq(serverEnrollment.id, attemptId), ownedBy(runId)), ending)) === 1;
});

export const checkServerEnrollment = Effect.fn("ServerEnrollment.check")(function* (attemptId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    tokenId: machineEnrollmentToken.id,
    joined: machineEnrollmentToken.joinedMachineId,
    lastCheckInAt: waitingServer.lastCheckInAt,
  })
    .from(serverEnrollment)
    .leftJoin(machineEnrollmentToken, eq(machineEnrollmentToken.id, serverEnrollment.enrollTokenId))
    .innerJoin(waitingServer, eq(waitingServer.id, serverEnrollment.waitingServerId))
    .where(eq(serverEnrollment.id, attemptId));
  if (row === undefined) return { kind: "gone" } as const;
  if (row.tokenId === null) return { kind: "removed" } as const;
  if (row.joined !== null) return { kind: "joined", machineId: row.joined } as const;
  if (Date.now() - row.lastCheckInAt.getTime() > CHECKIN_SILENCE_MS) return { kind: "silent" } as const;
  return { kind: "waiting" } as const;
});

const RUN_ENDS = {
  cancellation: { state: "cancelled" },
  failure: { state: "failed", failure: { stage: null, message: "Enrollment stopped" } },
} satisfies Record<RunEnd, Ending>;

export const closeServerEnrollment = Effect.fn("ServerEnrollment.closeRun")(function* (runId: string, end: RunEnd, attemptId?: string) {
  const unbound = attemptId === undefined
    ? undefined
    : and(eq(serverEnrollment.id, attemptId), isNull(serverEnrollment.inngestRunId), open);
  return yield* endAndRevokeTokens(or(ownedBy(runId), unbound), RUN_ENDS[end]);
});

// Each event's outbox row stays locked until commit, so every canceller enqueues only after it holds its attempts, in id order.
const enqueueCancels = (attemptIds: ReadonlyArray<string>) =>
  Effect.forEach([...attemptIds].sort(), (attemptId) => enqueueEvent(createServerEnrollmentCancelEvent({ attemptId })), { discard: true });

/**
 * Cancels the open attempts among `attemptIds`: a bound attempt's run ends it, and an unbound one ends here. Spares, and
 * returns, those whose join Cloud may have sent: cancelling sends no Leave, so that Server would stay in the Cluster.
 */
export const cancelServerEnrollments = Effect.fn("ServerEnrollment.cancelAttempts")(function* (
  organizationId: string,
  attemptIds: ReadonlyArray<string>,
) {
  if (attemptIds.length === 0) return [];
  const { drizzle } = yield* Database;
  const locked = yield* drizzle.select({ id: serverEnrollment.id, runId: serverEnrollment.inngestRunId, tokenId: serverEnrollment.enrollTokenId })
    .from(serverEnrollment)
    .where(and(eq(serverEnrollment.organizationId, organizationId), inArray(serverEnrollment.id, [...attemptIds]), open))
    .orderBy(serverEnrollment.id)
    .for("update");
  if (locked.length === 0) return [];
  // A join saves its assignment holding the token's row lock, then sends Register. Lock the tokens, then read in a new statement.
  const tokenIds = locked.map((row) => row.tokenId);
  yield* drizzle.select({ id: machineEnrollmentToken.id }).from(machineEnrollmentToken)
    .where(inArray(machineEnrollmentToken.id, tokenIds)).orderBy(machineEnrollmentToken.id).for("update");
  const sent = yield* joinsSent(organizationId, tokenIds);
  const cancelling = locked.filter((row) => !sent.has(row.tokenId));
  // Expired now, not when the run ends the attempt, so a join after this cancel finds the token dead.
  if (cancelling.length > 0) {
    yield* drizzle.update(machineEnrollmentToken).set({ expiresAt: sql`now()` }).where(and(
      inArray(machineEnrollmentToken.id, cancelling.map((row) => row.tokenId)),
      isNull(machineEnrollmentToken.joinedMachineId),
    ));
  }
  const unbound = cancelling.filter((row) => row.runId === null).map((row) => row.id);
  if (unbound.length > 0) yield* endAndRevokeTokensNow(and(inArray(serverEnrollment.id, unbound), open), { state: "cancelled" });
  yield* enqueueCancels(cancelling.map((row) => row.id));
  return locked.filter((row) => sent.has(row.tokenId)).map((row) => row.id);
});

/** The enroll tokens whose join Cloud may have sent: joined, or their Machine holds an assignment. */
export const joinsSent = Effect.fn("ServerEnrollment.joinsSent")(function* (
  organizationId: string,
  tokenIds: ReadonlyArray<string>,
) {
  const { drizzle } = yield* Database;
  // Only the current pairing's assignments: a Machine keeps its ID across a pairing that was forgotten. Only a claimed token has one.
  const [pairing] = yield* drizzle.select({ secret: organizationPairing.encryptedPairingSecret }).from(organizationPairing)
    .where(and(eq(organizationPairing.organizationId, organizationId), exists(drizzle.select({ one: sql`1` }).from(machineEnrollmentToken)
      .where(and(inArray(machineEnrollmentToken.id, [...tokenIds]), isNotNull(machineEnrollmentToken.claimedMachineId))))));
  const clusterKey = pairing === undefined ? null : hashEnrollmentToken(yield* decryptPairingSecret(pairing.secret));
  return new Set((yield* drizzle.select({ id: machineEnrollmentToken.id }).from(machineEnrollmentToken).where(and(
    inArray(machineEnrollmentToken.id, [...tokenIds]),
    or(isNotNull(machineEnrollmentToken.joinedMachineId), clusterKey === null ? undefined : exists(drizzle.select({ one: sql`1` }).from(enrollmentAllocation).where(and(
      eq(enrollmentAllocation.organizationId, machineEnrollmentToken.organizationId),
      eq(enrollmentAllocation.clusterKey, clusterKey),
      sql`${enrollmentAllocation.assignments} @> jsonb_build_array(jsonb_build_object('machine', jsonb_build_object('id', ${machineEnrollmentToken.claimedMachineId}::text)))`,
    )))),
  ))).map((row) => row.id));
});

export const cancelOpenServerEnrollments = Effect.fn("ServerEnrollment.cancelOpen")(function* (organizationId: string) {
  yield* enqueueCancels(yield* endAndRevokeTokensNow(and(eq(serverEnrollment.organizationId, organizationId), open), { state: "cancelled" }));
});

export const closeStaleServerEnrollments = Effect.fn("ServerEnrollment.closeStale")(function* () {
  const database = yield* Database;
  const now = Date.now();
  const failed = yield* endAndRevokeTokens(
    and(open, lt(serverEnrollment.requestedAt, new Date(now - ENROLLMENT_STALE_MS))),
    { state: "failed", failure: { stage: null, message: "Enrollment stopped" } },
  );
  const removed = yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const keptLongEnough = exists(drizzle.select({ id: serverEnrollment.id }).from(serverEnrollment).where(and(
      eq(serverEnrollment.waitingServerId, waitingServer.id),
      eq(serverEnrollment.state, "joined"),
      lt(serverEnrollment.endedAt, new Date(now - JOINED_WAITING_KEEP_MS)),
    )));
    const locked = yield* drizzle.select({ id: waitingServer.id }).from(waitingServer).where(keptLongEnough).orderBy(waitingServer.id).for("update");
    if (locked.length === 0) return [];
    return yield* drizzle.delete(waitingServer).where(and(
      inArray(waitingServer.id, locked.map((row) => row.id)),
      notExists(drizzle.select({ id: serverEnrollment.id }).from(serverEnrollment).where(and(
        eq(serverEnrollment.waitingServerId, waitingServer.id),
        open,
      ))),
    )).returning({ id: waitingServer.id });
  }));
  return { failed, removed: removed.length };
});

export const openAttemptFor = Effect.fn("ServerEnrollment.openFor")(function* (waitingServerId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    id: serverEnrollment.id,
    role: serverEnrollment.role,
    state: serverEnrollment.state,
    stage: serverEnrollment.stage,
    instruction: serverEnrollment.instruction,
    encryptedToken: serverEnrollment.encryptedToken,
    tokenId: serverEnrollment.enrollTokenId,
  }).from(serverEnrollment).where(and(eq(serverEnrollment.waitingServerId, waitingServerId), open, isNotNull(serverEnrollment.encryptedToken)));
  return row ?? null;
});

export type OpenAttempt = NonNullable<Effect.Success<ReturnType<typeof openAttemptFor>>>;
