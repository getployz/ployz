import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { and, asc, eq, inArray, isNull, gt, gte, max, or, sql } from "drizzle-orm";
import { Clock, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { createServerEnrollmentRequestedEvent } from "#/modules/inngest/events";
import { insertEnrollmentToken, readJoinStatus } from "#/modules/machines/enrollment.server";
import { isDeadFoundingClaim, lockOrganizationEnrollment, revokeOrganizationPairing, shareOrganizationEnrollment } from "#/modules/machines/pairing-removal.server";
import { organizationMachine } from "#/modules/machines/tables";
import type { StoredReport } from "#/modules/machines/waiting/checkin-wire";
import { cancelServerEnrollments } from "#/modules/machines/waiting/enrollment-run.server";
import {
  ENROLL_MIN_CLI_VERSION,
  type EnrollInput,
  type EnrollInstruction,
  type EnrollmentRole,
  type EnrollmentState,
  type EnrollStorage,
  isReadyFor,
} from "#/modules/machines/waiting/server-enrollment";
import { serversOffline } from "#/modules/machines/waiting/servers-offline";
import { fleetToken, fleetTokenAttempt, fleetTokenJoin, serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { observeIngestCursor } from "#/modules/observe-ingest/tables";
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
  readonly joined?: boolean;
  readonly tokenEnded: "expired" | "revoked" | null;
};

export type PlannedEnrollment = {
  readonly waitingServerId: string;
  readonly fleetTokenId: string;
  readonly hostname: string;
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
  if (row.joined) return "Already joined";
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
  if (spare === undefined) return { kind: "zfs" };
  const device = spare.byId.startsWith("/") ? spare.byId : `/dev/disk/by-id/${spare.byId}`;
  return { kind: "zfs", device };
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
    hostname: row.report.hostname,
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

const candidatesOf = Effect.fn("ServerEnrollment.candidates")(function* (organizationId: string, ids: ReadonlyArray<string>, lock: boolean) {
  const { drizzle } = yield* Database;
  const select = drizzle
    .select({
      id: waitingServer.id,
      fleetTokenId: waitingServer.fleetTokenId,
      report: waitingServer.report,
      observedIp: waitingServer.observedIp,
      rejected: sql<boolean>`${waitingServer.rejectedAt} is not null`,
      enrolling: sql<boolean>`exists (select 1 from ${serverEnrollment} where ${serverEnrollment.waitingServerId} = ${waitingServer.id} and ${serverEnrollment.endedAt} is null)`,
      joined: sql<boolean>`exists (select 1 from ${serverEnrollment} where ${serverEnrollment.waitingServerId} = ${waitingServer.id} and ${serverEnrollment.state} = 'joined')`,
      tokenEnded: sql<"expired" | "revoked" | null>`case when ${fleetToken.revokedAt} is not null then 'revoked' when ${fleetToken.expiresAt} <= now() then 'expired' end`,
    })
    .from(waitingServer)
    .innerJoin(fleetToken, eq(fleetToken.id, waitingServer.fleetTokenId))
    .where(and(eq(waitingServer.organizationId, organizationId), inArray(waitingServer.id, [...ids])))
    .orderBy(waitingServer.id);
  return lock ? yield* select.for("update", { of: waitingServer }) : yield* select;
});

const openFounderAttempt = Effect.fn("ServerEnrollment.openFounder")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [founding] = yield* drizzle
    .select({ id: serverEnrollment.id })
    .from(serverEnrollment)
    .where(and(eq(serverEnrollment.organizationId, organizationId), eq(serverEnrollment.role, "founder"), isNull(serverEnrollment.endedAt)));
  return founding;
});

const releaseDeadClaim = Effect.fn("ServerEnrollment.releaseDeadClaim")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select({ founderMachineId: organizationPairing.founderMachineId })
    .from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  if (pairing === undefined || pairing.founderMachineId !== null) return;
  if ((yield* revokeOrganizationPairing(organizationId, { deadClaimOnly: true })).confirmed) return;
  // A first server can't found while the earlier pairing waits on removal, so it ends now rather than failing hours later.
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    yield* lockOrganizationEnrollment(organizationId);
    const { drizzle } = yield* Database;
    const [current] = yield* drizzle.select({ removalStartedAt: organizationPairing.removalStartedAt })
      .from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId)).for("update");
    if (current?.removalStartedAt == null) return;
    const founding = yield* openFounderAttempt(organizationId);
    if (founding !== undefined) yield* cancelServerEnrollments(organizationId, [founding.id]);
  }));
});

const enrollmentAuthority = Effect.fn("ServerEnrollment.authority")(function* (
  organizationId: string,
  mode: "start" | "preview",
) {
  const { drizzle } = yield* Database;
  const select = drizzle.select().from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  const [persisted] = mode === "start" ? yield* select.for("update") : yield* select;
  const pairing = persisted?.founderMachineId === null && persisted.removalEndpoints?.every((endpoint) => endpoint.status === "confirmed")
    ? undefined
    : persisted;
  const founded = pairing !== undefined && pairing.founderMachineId !== null;
  const founding = founded ? undefined : yield* openFounderAttempt(organizationId);
  const paired = pairing !== undefined && !(mode === "preview" && (yield* isDeadFoundingClaim(pairing)));
  return { pairing, founded, hasFirst: paired || founding !== undefined };
});

/** Whether the Organization has a first server, founded or founding. */
export const hasFirstServer = Effect.fn("ServerEnrollment.hasFirst")(function* (organizationId: string) {
  return (yield* enrollmentAuthority(organizationId, "preview")).hasFirst;
});

const planEnrollment = Effect.fn("ServerEnrollment.plan")(function* (
  organizationId: string,
  input: EnrollInput,
  mode: "start" | "preview",
) {
  const { pairing, founded, hasFirst } = yield* enrollmentAuthority(organizationId, mode);
  if (pairing !== undefined && pairing.removalEndpoints !== null) {
    const retryable = pairing.removalEndpoints.some((endpoint) => endpoint.status === "pending");
    return yield* new Conflict({
      message: retryable
        ? "Cloud can't reach an earlier server to remove its pairing yet. Try again, or use Forget Servers."
        : "Cloud can't remove an earlier server's pairing. Use Forget Servers.",
      userFacing: true,
    });
  }
  const ids = input.ids.filter((id) => /^[0-9a-f-]{36}$/u.test(id));
  const plan = planServerEnrollment(
    input,
    ids.length === 0 ? [] : yield* candidatesOf(organizationId, ids, mode === "start"),
    { hasFirst },
  );
  if (plan.kind === "refused") return yield* new Validation({ message: plan.message, userFacing: true });
  return { ...plan, founded };
});

export const previewServerEnrollment = Effect.fn("ServerEnrollment.preview")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string } & EnrollInput,
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:add");
  const { rows, skipped, spread } = yield* planEnrollment(organization.id, input, "preview");
  return { rows, skipped, spread, serversOffline: serversOffline(yield* serverContacts(organization.id), yield* Clock.currentTimeMillis) };
});

/** Each Server of the Organization with its last signed push on any stream. */
const serverContacts = Effect.fn("ServerEnrollment.contacts")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({
    enrolledAt: organizationMachine.createdAt,
    pushTarget: organizationMachine.pushTarget,
    lastPushAt: max(observeIngestCursor.updatedAt),
  })
    .from(organizationMachine)
    .leftJoin(observeIngestCursor, and(
      eq(observeIngestCursor.organizationId, organizationMachine.organizationId),
      eq(observeIngestCursor.machineId, organizationMachine.machineId),
    ))
    .where(eq(organizationMachine.organizationId, organizationId))
    .groupBy(organizationMachine.organizationId, organizationMachine.machineId);
});

export const startServerEnrollment = Effect.fn("ServerEnrollment.start")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string } & EnrollInput,
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:add");
  const database = yield* Database;
  yield* releaseDeadClaim(organization.id);
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockOrganizationEnrollment(organization.id);
    const plan = yield* planEnrollment(organization.id, input, "start");
    return yield* startPlannedEnrollment(organization.id, actor.userId, plan);
  })).pipe(
    Effect.catchIf(isUniqueViolation, () =>
      Effect.fail(new Conflict({ message: "Another enrollment just started for these servers, or another first server.", userFacing: true }))),
  );
});

const startPlannedEnrollment = Effect.fn("ServerEnrollment.startPlanned")(function* (
  organizationId: string,
  userId: string,
  plan: Extract<EnrollPlan, { kind: "planned" }> & { readonly founded: boolean },
  delivery: "await" | "detach" = "await",
) {
  const encryption = yield* SecretEncryption;
  const { drizzle } = yield* Database;
  const attempts = [];
  for (const row of plan.rows) {
    const id = randomUUID();
    const issued = yield* insertEnrollmentToken({
      organizationId,
      userId,
      attempt: { role: row.role, serverEnrollmentId: id },
    });
    const state: EnrollmentState = row.role === "founder" || plan.founded ? "instructed" : "awaiting_founder";
    yield* drizzle.insert(serverEnrollment).values({
      id,
      organizationId,
      waitingServerId: row.waitingServerId,
      fleetTokenId: row.fleetTokenId,
      role: row.role,
      instruction: row.instruction,
      enrollTokenId: issued.id,
      encryptedToken: encryption.encrypt(issued.token),
      state,
      requestedByUserId: userId,
    });
    yield* drizzle.insert(fleetTokenAttempt)
      .select(drizzle.select({ organizationId: waitingServer.organizationId, fleetTokenId: sql`${row.fleetTokenId}::uuid`.as("fleet_token_id"), checkinKey: waitingServer.checkinKey })
        .from(waitingServer).where(eq(waitingServer.id, row.waitingServerId)))
      .onConflictDoNothing();
    yield* enqueueEvent(createServerEnrollmentRequestedEvent({ attemptId: id, organizationId }), delivery);
    attempts.push({ id, waitingServerId: row.waitingServerId, role: row.role, name: row.instruction.name, state });
  }
  return { attempts, skipped: plan.skipped, spread: plan.spread };
});

const usedOf = (tokenId: string) => sql<number>`(select count(*) from ${fleetTokenJoin} where ${fleetTokenJoin.fleetTokenId} = ${tokenId})
  + (select count(*) from ${serverEnrollment} where ${serverEnrollment.fleetTokenId} = ${tokenId} and ${serverEnrollment.endedAt} is null)`.mapWith(Number);

// A host arrives once per token: after any attempt under it, even one whose waiting row has since gone, it waits for a person.
// The row's own enrollments still count, since an app older than the attempt marker may start one during an upgrade.
const arrivalSpent = (tokenId: string) => sql<boolean>`exists (select 1 from ${fleetTokenAttempt} where ${fleetTokenAttempt.fleetTokenId} = ${tokenId}
  and ${fleetTokenAttempt.checkinKey} = ${waitingServer.checkinKey})
  or exists (select 1 from ${serverEnrollment} where ${serverEnrollment.waitingServerId} = ${waitingServer.id}
  and (${serverEnrollment.fleetTokenId} = ${tokenId} or ${serverEnrollment.state} = 'joined' or ${serverEnrollment.endedAt} is null))`;

const arrivingHost = (token: { readonly id: string; readonly organizationId: string }, checkinKey: string) => and(
  eq(waitingServer.organizationId, token.organizationId), eq(waitingServer.checkinKey, checkinKey),
  eq(waitingServer.fleetTokenId, token.id), isNull(waitingServer.rejectedAt),
);

/**
 * Starts the enroll of a ready host that just checked in on an Enroll on arrival token, while the token's used count
 * is under its max. Each host gets one such attempt per token; a manual enroll is the retry. An unlocked read first skips hosts that cannot start, so only a likely start opens a transaction.
 * Arrivals lock the token before the host, the same order as check-in and revoke, so the locks never form a cycle.
 * The token lock also queues arrivals of one token, and the host read after it sees every attempt that committed ahead.
 */
export const startArrivalEnrollment = Effect.fn("ServerEnrollment.startArrival")(function* (
  token: { readonly id: string; readonly organizationId: string; readonly autoEnrollMax: number },
  checkinKey: string,
) {
  const database = yield* Database;
  const [glance] = yield* database.drizzle.select({ unavailable: arrivalSpent(token.id), used: usedOf(token.id) })
    .from(waitingServer).where(arrivingHost(token, checkinKey));
  if (glance === undefined || glance.unavailable || glance.used >= token.autoEnrollMax) return false;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // Every pairing write that matters here holds the exclusive Organization lock, so the shared one keeps the row still.
    yield* shareOrganizationEnrollment(token.organizationId);
    const [pairing] = yield* drizzle.select({ founder: organizationPairing.founderMachineId, removal: organizationPairing.removalEndpoints })
      .from(organizationPairing).where(eq(organizationPairing.organizationId, token.organizationId));
    if (pairing === undefined || pairing.founder === null || pairing.removal !== null) return false;
    const [live] = yield* drizzle.select({ max: fleetToken.autoEnrollMax, userId: fleetToken.createdByUserId })
      .from(fleetToken).where(and(eq(fleetToken.id, token.id), isNull(fleetToken.revokedAt), gt(fleetToken.expiresAt, sql`now()`))).for("no key update");
    if (live === undefined || live.max === null) return false;
    const [host] = yield* drizzle.select({
      id: waitingServer.id, report: waitingServer.report, observedIp: waitingServer.observedIp,
      unavailable: arrivalSpent(token.id), used: usedOf(token.id),
    }).from(waitingServer).where(arrivingHost(token, checkinKey)).for("update");
    if (host === undefined || host.unavailable || host.used >= live.max) return false;
    const { report } = host;
    if (report.blockers.length > 0 || (report.prep !== undefined && (report.prep.docker !== "ready" || report.prep.zfs === "installing"))) return false;
    const input: EnrollInput = {
      ids: [host.id], names: { kind: "hostnames" },
      storage: report.prep !== undefined && report.prep.zfs !== "ready"
        ? { kind: "none" }
        : { kind: "zfs", useSpareDisk: true, fallbackToDocker: true },
      runs: { services: true, ingress: true, builds: true },
    };
    const plan = planServerEnrollment(input, [{
      id: host.id, fleetTokenId: token.id, report, observedIp: host.observedIp,
      rejected: false, enrolling: false, tokenEnded: null,
    }], { hasFirst: true });
    if (plan.kind !== "planned" || plan.rows.length === 0) return false;
    // A check-in answers before Inngest hears of the attempt; the outbox sweep sends any delivery that fails.
    yield* startPlannedEnrollment(token.organizationId, live.userId, { ...plan, founded: true }, "detach");
    return true;
  }));
});

export const cancelServerEnrollment = Effect.fn("ServerEnrollment.cancel")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly id: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:add");
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [attempt] = yield* drizzle
      .select({ id: serverEnrollment.id, open: sql<boolean>`${serverEnrollment.endedAt} is null` })
      .from(serverEnrollment)
      .where(and(eq(serverEnrollment.organizationId, organization.id), eq(serverEnrollment.id, input.id)));
    if (attempt === undefined) return yield* new NotFound({ message: "No such enrollment." });
    if (attempt.open) yield* cancelServerEnrollments(organization.id, [attempt.id]);
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
  /** Waiting for a join slot: the joins that must finish before it goes. Null when it isn't in the join line. */
  readonly ahead: number | null;
  readonly machineId: string | null;
  readonly failure: { readonly stage: string | null; readonly message: string } | null;
  readonly requestedAt: string;
  readonly endedAt: string | null;
};

export const RECENT_ENROLLMENT_MS = 60 * 60 * 1000;

export const readServerEnrollmentStatus = Effect.fn("ServerEnrollment.status")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly id?: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
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
      input.id === undefined ? and(isNull(waitingServer.rejectedAt), or(
        isNull(serverEnrollment.endedAt), gte(serverEnrollment.endedAt, new Date(Date.now() - RECENT_ENROLLMENT_MS)),
      )) : eq(serverEnrollment.id, input.id),
    ))
    .orderBy(asc(serverEnrollment.requestedAt), asc(waitingServer.firstSeenAt), asc(waitingServer.id), asc(serverEnrollment.id));
  const { ahead, joining } = yield* readJoinStatus(organization.id);
  const { hasFirst } = yield* enrollmentAuthority(organization.id, "preview");
  const attempts = rows.map(({ instruction, report, ...row }): ServerEnrollmentView => ({
    ...row,
    name: instruction.name,
    storage: instruction.storage,
    queued: row.state === "instructed" && !isReadyFor(report, instruction.storage),
    ahead: row.endedAt === null ? ahead.get(row.id) ?? null : null,
    requestedAt: row.requestedAt.toISOString(),
    endedAt: row.endedAt?.toISOString() ?? null,
  }));
  return { attempts, hasFirst, joining };
});

export const readServerEnrollments = Effect.fn("ServerEnrollment.read")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly id?: string },
) {
  return (yield* readServerEnrollmentStatus(actor, input)).attempts;
});
