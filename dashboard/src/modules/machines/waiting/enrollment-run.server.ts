import "@tanstack/react-start/server-only";
import { and, eq, exists, inArray, isNotNull, isNull, lt, notExists, or } from "drizzle-orm";
import { Effect } from "effect";
import type { RunEnd } from "#/modules/inngest/attempt-lifecycle";
import type { MachineId } from "#/db/tables";
import { machineEnrollmentToken } from "#/modules/machines/tables";
import { ENROLLMENT_STAGES, type EnrollmentFailure, type EnrollmentStage } from "#/modules/machines/waiting/server-enrollment";
import { serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
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

export const recordStage = Effect.fn("ServerEnrollment.recordStage")(function* (attemptId: string, runId: string, stage: string | null) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.update(serverEnrollment).set({ state: "running", stage: asStage(stage) }).where(and(
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
  Effect.flatMap(Database, (database) => database.transaction(endAndRevokeTokensNow(where, ending)));

const endAndRevokeTokensNow = Effect.fn("ServerEnrollment.end")(function* (where: ReturnType<typeof and>, ending: Ending) {
  const { drizzle } = yield* Database;
  const now = new Date();
  const ended = yield* drizzle.update(serverEnrollment).set({
    state: ending.state,
    machineId: ending.state === "joined" ? ending.machineId : null,
    failure: ending.state === "failed" ? ending.failure : null,
    encryptedToken: null,
    endedAt: now,
  }).where(where).returning({ id: serverEnrollment.id, enrollTokenId: serverEnrollment.enrollTokenId });
  if (ending.state !== "joined" && ended.length > 0) {
    yield* drizzle.update(machineEnrollmentToken).set({ expiresAt: now }).where(and(
      inArray(machineEnrollmentToken.id, ended.map((row) => row.enrollTokenId)),
      isNull(machineEnrollmentToken.joinedMachineId),
    ));
  }
  return ended.length;
});

export const endServerEnrollment = Effect.fn("ServerEnrollment.endRun")(function* (attemptId: string, runId: string, ending: Ending) {
  return (yield* endAndRevokeTokens(and(eq(serverEnrollment.id, attemptId), ownedBy(runId)), ending)) === 1;
});

export const checkServerEnrollment = Effect.fn("ServerEnrollment.check")(function* (attemptId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    joined: machineEnrollmentToken.joinedMachineId,
    lastCheckInAt: waitingServer.lastCheckInAt,
  })
    .from(serverEnrollment)
    .innerJoin(machineEnrollmentToken, eq(machineEnrollmentToken.id, serverEnrollment.enrollTokenId))
    .innerJoin(waitingServer, eq(waitingServer.id, serverEnrollment.waitingServerId))
    .where(eq(serverEnrollment.id, attemptId));
  if (row === undefined) return { kind: "gone" } as const;
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

export const closeUnboundServerEnrollments = Effect.fn("ServerEnrollment.closeUnbound")(function* (
  organizationId: string,
  attemptIds: ReadonlyArray<string>,
) {
  return yield* endAndRevokeTokens(and(
    eq(serverEnrollment.organizationId, organizationId),
    inArray(serverEnrollment.id, [...attemptIds]),
    isNull(serverEnrollment.inngestRunId),
    open,
  ), { state: "cancelled" });
});

export const closeStaleServerEnrollments = Effect.fn("ServerEnrollment.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const failed = yield* endAndRevokeTokens(
    and(open, lt(serverEnrollment.requestedAt, new Date(now - ENROLLMENT_STALE_MS))),
    { state: "failed", failure: { stage: null, message: "Enrollment stopped" } },
  );
  const removed = yield* drizzle.delete(waitingServer).where(and(
    notExists(drizzle.select({ id: serverEnrollment.id }).from(serverEnrollment).where(and(
      eq(serverEnrollment.waitingServerId, waitingServer.id),
      open,
    ))),
    exists(drizzle.select({ id: serverEnrollment.id }).from(serverEnrollment).where(and(
      eq(serverEnrollment.waitingServerId, waitingServer.id),
      eq(serverEnrollment.state, "joined"),
      lt(serverEnrollment.endedAt, new Date(now - JOINED_WAITING_KEEP_MS)),
    ))),
  )).returning({ id: waitingServer.id });
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
  }).from(serverEnrollment).where(and(eq(serverEnrollment.waitingServerId, waitingServerId), open, isNotNull(serverEnrollment.encryptedToken)));
  return row ?? null;
});

export type OpenAttempt = NonNullable<Effect.Success<ReturnType<typeof openAttemptFor>>>;
