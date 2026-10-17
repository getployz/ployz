import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { and, asc, eq, inArray, isNotNull, isNull, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { createServerEnrollmentCancelEvent, createServerEnrollmentRequestedEvent } from "#/modules/inngest/events";
import type { MachineId } from "#/db/tables";
import { foundingClaimFresh } from "#/modules/machines/connections.server";
import { insertEnrollmentToken } from "#/modules/machines/enrollment.server";
import { enrollmentAllocation, machineEnrollmentToken, organizationMachine } from "#/modules/machines/tables";
import type { StoredReport } from "#/modules/machines/waiting/checkin-wire";
import { closeUnboundServerEnrollments } from "#/modules/machines/waiting/enrollment-run.server";
import {
  ENROLL_MIN_CLI_VERSION,
  type EnrollInput,
  type EnrollInstruction,
  type EnrollmentRole,
  type EnrollmentState,
  type EnrollStorage,
  isReadyFor,
} from "#/modules/machines/waiting/server-enrollment";
import { fleetToken, serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { requireAccess } from "#/modules/organization/access.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { isBehind } from "#/modules/server-upgrade/server-upgrade";
import { Database, isUniqueViolation } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

export type EnrollCandidate = {
  readonly id: string;
  readonly fleetTokenId: string;
  readonly report: StoredReport;
  readonly observedIp: string;
  readonly rejected: boolean;
  readonly enrolling: boolean;
  readonly tokenEnded: "expired" | "revoked" | null;
};

export type PlannedEnrollment = {
  readonly waitingServerId: string;
  readonly fleetTokenId: string;
  readonly role: EnrollmentRole;
  readonly instruction: EnrollInstruction;
};

export type EnrollPlan =
  | {
    readonly kind: "planned";
    readonly rows: ReadonlyArray<PlannedEnrollment>;
    readonly skipped: ReadonlyArray<{ readonly id: string; readonly reason: string }>;
    readonly spread: { readonly providers: number; readonly networks: number };
  }
  | { readonly kind: "refused"; readonly message: string };

function skipReason(row: EnrollCandidate | undefined): string | null {
  if (row === undefined || row.rejected) return "Not waiting";
  if (row.enrolling) return "Already enrolling";
  if (row.tokenEnded !== null) return `Fleet Token ${row.tokenEnded}`;
  const blocker = row.report.blockers[0];
  if (blocker !== undefined) return blocker.message;
  if (row.report.cliVersion === undefined || isBehind(row.report.cliVersion, ENROLL_MIN_CLI_VERSION)) return "Update ployz";
  return null;
}

function resolveStorage(report: StoredReport, storage: EnrollInput["storage"]): EnrollStorage | { readonly skip: string } {
  if (storage.kind === "none") return { kind: "none" };
  if (report.zfs.kind === "ineligible") return storage.fallbackToDocker ? { kind: "none" } : { skip: report.zfs.message };
  const spare = storage.useSpareDisk ? report.zfs.targets.find((target) => target.kind === "disk") : undefined;
  return spare === undefined ? { kind: "zfs" } : { kind: "zfs", device: spare.byId };
}

function network(ip: string) {
  const address = ip.replace(/^::ffff:/u, "");
  const v4 = /^(\d+)\.(\d+)\.\d+\.\d+$/u.exec(address);
  return v4 === null ? address.split(":").slice(0, 2).join(":") : `${v4[1]}.${v4[2]}`;
}

export function planServerEnrollment(
  input: EnrollInput,
  candidates: ReadonlyArray<EnrollCandidate>,
  organization: { readonly hasFirst: boolean },
): EnrollPlan {
  const byId = new Map(candidates.map((row) => [row.id, row]));
  const ids = [...new Set(input.ids)];
  const skipped: Array<{ id: string; reason: string }> = [];
  const eligible: Array<{ row: EnrollCandidate; storage: EnrollStorage }> = [];
  for (const id of ids) {
    const row = byId.get(id);
    const reason = skipReason(row);
    if (row === undefined || reason !== null) {
      skipped.push({ id, reason: reason ?? "Not waiting" });
      continue;
    }
    const storage = resolveStorage(row.report, input.storage);
    if ("skip" in storage) skipped.push({ id, reason: storage.skip });
    else eligible.push({ row, storage });
  }

  let founder: string | null = null;
  if (!organization.hasFirst) {
    if (input.first !== undefined && !ids.includes(input.first)) {
      return { kind: "refused", message: "The first server must be one of the servers you enroll." };
    }
    const pick = input.first ?? (eligible.length === 1 && ids.length === 1 ? eligible[0]?.row.id : undefined);
    if (pick === undefined) return { kind: "refused", message: "Pick the first server: it sets up the cluster the rest join." };
    const skip = skipped.find((row) => row.id === pick);
    if (skip !== undefined) return { kind: "refused", message: `The first server can't enroll: ${skip.reason}.` };
    founder = pick;
  }
  const ordered = founder === null
    ? eligible
    : [...eligible.filter(({ row }) => row.id === founder), ...eligible.filter(({ row }) => row.id !== founder)];

  const { names } = input;
  if (names.kind === "pattern" && !names.pattern.includes("{n}") && ordered.length > 1) {
    return { kind: "refused", message: "A name pattern needs {n} to name more than one server." };
  }
  const policy = {
    labels: {},
    acceptsBuilds: input.runs.builds,
    acceptsServices: input.runs.services,
    acceptsIngress: input.runs.ingress,
  };
  const rows = ordered.map(({ row, storage }, index): PlannedEnrollment => ({
    waitingServerId: row.id,
    fleetTokenId: row.fleetTokenId,
    role: row.id === founder ? "founder" : "joiner",
    instruction: {
      name: names.kind === "hostnames" ? row.report.hostname : names.pattern.replaceAll("{n}", String(index + 1)),
      storage,
      policy,
    },
  }));
  return {
    kind: "planned",
    rows,
    skipped,
    spread: {
      providers: new Set(ordered.map(({ row }) => row.report.provider ?? "Unknown")).size,
      networks: new Set(ordered.map(({ row }) => network(row.observedIp))).size,
    },
  };
}

const candidatesOf = Effect.fn("ServerEnrollment.candidates")(function* (organizationId: string, ids: ReadonlyArray<string>) {
  const { drizzle } = yield* Database;
  return yield* drizzle
    .select({
      id: waitingServer.id,
      fleetTokenId: waitingServer.fleetTokenId,
      report: waitingServer.report,
      observedIp: waitingServer.observedIp,
      rejected: sql<boolean>`${waitingServer.rejectedAt} is not null`,
      enrolling: sql<boolean>`exists (select 1 from ${serverEnrollment} where ${serverEnrollment.waitingServerId} = ${waitingServer.id} and ${serverEnrollment.endedAt} is null)`,
      tokenEnded: sql<"expired" | "revoked" | null>`case when ${fleetToken.revokedAt} is not null then 'revoked' when ${fleetToken.expiresAt} <= now() then 'expired' end`,
    })
    .from(waitingServer)
    .innerJoin(fleetToken, eq(fleetToken.id, waitingServer.fleetTokenId))
    .where(and(eq(waitingServer.organizationId, organizationId), inArray(waitingServer.id, [...ids])))
    .for("update", { of: waitingServer });
});

/**
 * Clears a Founding Claim no Server can finish, so a new first server can found: one an ended attempt took, or an old
 * one. A fresh claim from `ployz server add` is still joining and stays.
 */
const releaseDeadClaim = Effect.fn("ServerEnrollment.releaseDeadClaim")(function* (
  organizationId: string,
  claim: { readonly founderClaimMachineId: MachineId; readonly createdAt: Date; readonly removalStartedAt: Date | null },
) {
  const { drizzle } = yield* Database;
  if (claim.removalStartedAt !== null) return false;
  const [attemptClaim] = yield* drizzle
    .select({ id: machineEnrollmentToken.id })
    .from(machineEnrollmentToken)
    .where(and(
      eq(machineEnrollmentToken.organizationId, organizationId),
      eq(machineEnrollmentToken.claimedMachineId, claim.founderClaimMachineId),
      isNotNull(machineEnrollmentToken.serverEnrollmentId),
    ))
    .limit(1);
  if (attemptClaim === undefined && foundingClaimFresh({ founderMachineId: null, createdAt: claim.createdAt })) return false;
  yield* drizzle.delete(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
  yield* drizzle.delete(enrollmentAllocation).where(eq(enrollmentAllocation.organizationId, organizationId));
  yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  return true;
});

export const startServerEnrollment = Effect.fn("ServerEnrollment.start")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string } & EnrollInput,
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const database = yield* Database;
  const encryption = yield* SecretEncryption;
  const ids = input.ids.filter((id) => /^[0-9a-f-]{36}$/u.test(id));
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [pairing] = yield* drizzle
      .select({
        founderMachineId: organizationPairing.founderMachineId,
        founderClaimMachineId: organizationPairing.founderClaimMachineId,
        createdAt: organizationPairing.createdAt,
        removalStartedAt: organizationPairing.removalStartedAt,
      })
      .from(organizationPairing)
      .where(eq(organizationPairing.organizationId, organization.id))
      .for("update");
    const founded = pairing !== undefined && pairing.founderMachineId !== null;
    const [founding] = founded ? [] : yield* drizzle
      .select({ id: serverEnrollment.id })
      .from(serverEnrollment)
      .where(and(eq(serverEnrollment.organizationId, organization.id), eq(serverEnrollment.role, "founder"), isNull(serverEnrollment.endedAt)));
    const claimed = pairing !== undefined && !founded && founding === undefined
      && !(yield* releaseDeadClaim(organization.id, pairing));
    const plan = planServerEnrollment(
      input,
      ids.length === 0 ? [] : yield* candidatesOf(organization.id, ids),
      { hasFirst: founded || founding !== undefined || claimed },
    );
    if (plan.kind === "refused") return yield* new Validation({ message: plan.message, userFacing: true });
    const attempts = [];
    for (const row of plan.rows) {
      const id = randomUUID();
      const issued = yield* insertEnrollmentToken({
        organizationId: organization.id,
        userId: actor.userId,
        attempt: { role: row.role, serverEnrollmentId: id },
      });
      const state: EnrollmentState = row.role === "founder" || founded ? "instructed" : "awaiting_founder";
      yield* drizzle.insert(serverEnrollment).values({
        id,
        organizationId: organization.id,
        waitingServerId: row.waitingServerId,
        fleetTokenId: row.fleetTokenId,
        role: row.role,
        instruction: row.instruction,
        enrollTokenId: issued.id,
        encryptedToken: encryption.encrypt(issued.token),
        state,
        requestedByUserId: actor.userId,
      });
      yield* enqueueEvent(createServerEnrollmentRequestedEvent({ attemptId: id, organizationId: organization.id }));
      attempts.push({ id, waitingServerId: row.waitingServerId, role: row.role, name: row.instruction.name, state });
    }
    return { attempts, skipped: plan.skipped, spread: plan.spread };
  })).pipe(
    Effect.catchIf(isUniqueViolation, () =>
      Effect.fail(new Conflict({ message: "Another enrollment just started for these servers, or another first server.", userFacing: true }))),
  );
});

export const cancelAttempts = Effect.fn("ServerEnrollment.cancelAttempts")(function* (
  organizationId: string,
  attemptIds: ReadonlyArray<string>,
) {
  if (attemptIds.length === 0) return;
  for (const attemptId of attemptIds) yield* enqueueEvent(createServerEnrollmentCancelEvent({ attemptId }));
  yield* closeUnboundServerEnrollments(organizationId, attemptIds);
});

export const cancelServerEnrollment = Effect.fn("ServerEnrollment.cancel")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly id: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [attempt] = yield* drizzle
      .select({ id: serverEnrollment.id, open: sql<boolean>`${serverEnrollment.endedAt} is null` })
      .from(serverEnrollment)
      .where(and(eq(serverEnrollment.organizationId, organization.id), eq(serverEnrollment.id, input.id)));
    if (attempt === undefined) return yield* new NotFound({ message: "No such enrollment." });
    if (attempt.open) yield* cancelAttempts(organization.id, [attempt.id]);
    return { id: attempt.id, cancelling: attempt.open };
  }));
});

export type ServerEnrollmentView = {
  readonly id: string;
  readonly waitingServerId: string;
  readonly role: EnrollmentRole;
  readonly name: string;
  readonly storage: EnrollStorage;
  readonly state: EnrollmentState;
  readonly stage: string | null;
  /** Instructed, but its host isn't ready for its storage yet: Cloud keeps answering `wait`. */
  readonly queued: boolean;
  readonly machineId: string | null;
  readonly failure: { readonly stage: string | null; readonly message: string } | null;
  readonly requestedAt: string;
  readonly endedAt: string | null;
};

export const readServerEnrollments = Effect.fn("ServerEnrollment.read")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly id?: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({
      id: serverEnrollment.id,
      waitingServerId: serverEnrollment.waitingServerId,
      role: serverEnrollment.role,
      instruction: serverEnrollment.instruction,
      state: serverEnrollment.state,
      stage: serverEnrollment.stage,
      machineId: serverEnrollment.machineId,
      failure: serverEnrollment.failure,
      requestedAt: serverEnrollment.requestedAt,
      endedAt: serverEnrollment.endedAt,
      report: waitingServer.report,
    })
    .from(serverEnrollment)
    .innerJoin(waitingServer, eq(waitingServer.id, serverEnrollment.waitingServerId))
    .where(and(
      eq(serverEnrollment.organizationId, organization.id),
      input.id === undefined ? isNull(waitingServer.rejectedAt) : eq(serverEnrollment.id, input.id),
    ))
    .orderBy(asc(serverEnrollment.requestedAt), asc(serverEnrollment.id));
  return rows.map(({ instruction, report, ...row }): ServerEnrollmentView => ({
    ...row,
    name: instruction.name,
    storage: instruction.storage,
    queued: row.state === "instructed" && !isReadyFor(report, instruction.storage),
    requestedAt: row.requestedAt.toISOString(),
    endedAt: row.endedAt?.toISOString() ?? null,
  }));
});
