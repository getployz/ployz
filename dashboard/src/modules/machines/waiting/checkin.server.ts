import "@tanstack/react-start/server-only";
import { createHash, createPublicKey, verify } from "node:crypto";
import { and, count, eq, exists, isNull, lt, not, or, sql, type SQL } from "drizzle-orm";
import { Clock, Data, Effect, Option, Schema } from "effect";
import {
  CHECKIN_WAIT_SECONDS,
  CheckinKey,
  CheckinRequest,
  type CheckinResponse,
  type CheckinStopReason,
  type StoredReport,
} from "#/modules/machines/waiting/checkin-wire";
import { createServerEnrollmentReportedEvent, type ServerEnrollmentCheckinReport } from "#/modules/inngest/events";
import { sendInngestEvent } from "#/modules/inngest/client";
import { asStage, forwardTo, joinsSent, openAttemptFor, type OpenAttempt } from "#/modules/machines/waiting/enrollment-run.server";
import { hasFirstServer, startArrivalEnrollment } from "#/modules/machines/waiting/enroll.server";
import { organizationMachine } from "#/modules/machines/tables";
import { hashFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { decodeStoredProbe, handOutProbe, probeAfterCheckIn } from "#/modules/machines/waiting/probe.server";
import { countWithinRateLimit, sometimesPruneStaleRateLimits } from "#/modules/machines/waiting/rate-limit.server";
import { PAST_STORAGE, isReadyFor, joinCommitted, ZFS_BUILD_FAILED } from "#/modules/machines/waiting/server-enrollment";
import { fleetToken, serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

export const WAITING_PER_TOKEN = 1000;
const LAST_SLOTS = 100;
const PER_TOKEN_PER_SECOND = 300;
const PER_IP_PER_SECOND = 50;

const MAX_SKEW_SECONDS = 300n;
const TIMESTAMP = /^(0|[1-9]\d{0,11})$/;
const SIGNATURE = /^v1=([A-Za-z0-9_-]{86})$/;
const BEARER = /^Bearer (pflt_\S+)$/u;
const DOMAIN = Buffer.from("ployz-checkin-v1\0", "ascii");

export class CheckinRefused extends Data.TaggedError("CheckinRefused")<{
  readonly status: 400 | 429;
  readonly message: string;
}> {}

const STOP_MESSAGES = {
  rejected: "This server was rejected in Ployz Cloud.",
  token_expired: "The Fleet Token expired.",
  token_revoked: "The Fleet Token was revoked.",
} satisfies Record<CheckinStopReason, string>;

const stop = (reason: CheckinStopReason): CheckinResponse => ({ kind: "stop", reason, message: STOP_MESSAGES[reason] });

/** The bytes a host signs: domain, Check-in Key, a 0 byte, unix seconds as i64 big-endian, and the body's sha256. */
export function checkinMessage(checkinKey: string, timestamp: bigint, body: Uint8Array) {
  const stamp = Buffer.alloc(8);
  stamp.writeBigInt64BE(timestamp);
  return Buffer.concat([
    DOMAIN,
    Buffer.from(checkinKey, "ascii"),
    Buffer.from([0]),
    stamp,
    createHash("sha256").update(body).digest(),
  ]);
}

function verifyCheckin(checkinKey: string, message: Uint8Array, signature: Uint8Array) {
  try {
    const x = Buffer.from(checkinKey, "hex").toString("base64url");
    return verify(null, message, createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x }, format: "jwk" }), signature);
  } catch {
    return false;
  }
}

const decodeRequest = Schema.decodeUnknownOption(CheckinRequest);
const decodeClaimedKey = Schema.decodeUnknownOption(Schema.Struct({ checkinKey: CheckinKey }));

/** The signature covers the raw body, so only the Check-in Key is read before it is checked. */
function claimedKey(body: Uint8Array) {
  try {
    const json: unknown = JSON.parse(new TextDecoder().decode(body));
    return Option.map(decodeClaimedKey(json), ({ checkinKey }) => ({ json, checkinKey }));
  } catch {
    return Option.none();
  }
}

const lockTokenAgainstRevoke = Effect.fn("Checkin.lockToken")(function* (id: string, mode: "share" | "update") {
  const { drizzle } = yield* Database;
  const [live] = yield* drizzle
    .select({
      revoked: sql<boolean>`${fleetToken.revokedAt} is not null`,
      expired: sql<boolean>`${fleetToken.expiresAt} <= now()`,
    })
    .from(fleetToken)
    .where(eq(fleetToken.id, id))
    .for(mode);
  return live;
});

const heldByToken = Effect.fn("Checkin.heldByToken")(function* (fleetTokenId: string) {
  const { drizzle } = yield* Database;
  const [held] = yield* drizzle
    .select({ rows: count() })
    .from(waitingServer)
    .where(eq(waitingServer.fleetTokenId, fleetTokenId));
  return held?.rows ?? 0;
});

const OWNERSHIP_CHANGED = Symbol("ownershipChanged");

const endedBy = (live: { readonly revoked: boolean; readonly expired: boolean } | undefined) =>
  live === undefined || live.revoked ? "token_revoked" : live.expired ? "token_expired" : null;

// An ended token stops its host only once no Enroll holds an attempt for it. Locking the row first waits out an Enroll
// that holds it, and the read after the lock sees what that Enroll committed.
const endUnlessEnrolling = Effect.fn("Checkin.endUnlessEnrolling")(function* (
  token: { readonly id: string; readonly organizationId: string },
  checkinKey: string,
  reason: CheckinStopReason,
) {
  const { drizzle } = yield* Database;
  const mine = and(
    eq(waitingServer.organizationId, token.organizationId),
    eq(waitingServer.checkinKey, checkinKey),
    eq(waitingServer.fleetTokenId, token.id),
  );
  yield* drizzle.select({ id: waitingServer.id }).from(waitingServer).where(mine).for("update");
  if (yield* hasOpenEnrollment(token, checkinKey)) return null;
  yield* drizzle.delete(waitingServer).where(mine);
  return stop(reason);
});

// A joined host checks in again only after its Server was reset. Once Cloud no longer holds that Server, the row starts
// over, as the seven-day sweep would remove it, so the host waits like any arrival. Its token's joins and spent arrival
// outlive the row.
const startOverAfterServerGone = Effect.fn("Checkin.startOverAfterServerGone")(function* (waitingServerId: string) {
  const { drizzle } = yield* Database;
  const joined = (server: SQL) => exists(drizzle.select({ id: serverEnrollment.id }).from(serverEnrollment).where(and(
    eq(serverEnrollment.waitingServerId, waitingServer.id),
    eq(serverEnrollment.state, "joined"),
    server,
  )));
  const serverHeld = exists(drizzle.select({ id: organizationMachine.machineId }).from(organizationMachine).where(and(
    eq(organizationMachine.organizationId, waitingServer.organizationId),
    eq(organizationMachine.machineId, serverEnrollment.machineId),
  )));
  const removed = yield* drizzle.delete(waitingServer).where(and(
    eq(waitingServer.id, waitingServerId),
    isNull(waitingServer.rejectedAt),
    joined(sql`true`),
    not(joined(serverHeld)),
  )).returning({ id: waitingServer.id });
  return removed.length > 0;
});

const admit = Effect.fn("Checkin.admit")(function* (
  token: { readonly id: string; readonly organizationId: string },
  request: CheckinRequest,
  observedIp: string,
) {
  const { drizzle } = yield* Database;
  const key = and(
    eq(waitingServer.organizationId, token.organizationId),
    eq(waitingServer.checkinKey, request.checkinKey),
  );
  const [found] = yield* drizzle
    .select({
      id: waitingServer.id,
      fleetTokenId: waitingServer.fleetTokenId,
      rejectedAt: waitingServer.rejectedAt,
      reportedAttemptId: waitingServer.reportedAttemptId,
      reportedStage: waitingServer.reportedStage,
    })
    .from(waitingServer)
    .where(key);
  const existing = found !== undefined && (yield* startOverAfterServerGone(found.id)) ? undefined : found;
  const newToToken = existing?.fleetTokenId !== token.id;
  const heldBefore = newToToken ? yield* heldByToken(token.id) : 0;
  const takingLastSlots = heldBefore >= WAITING_PER_TOKEN - LAST_SLOTS;
  const live = yield* lockTokenAgainstRevoke(token.id, takingLastSlots ? "update" : "share");
  const ended = endedBy(live);
  const stopped = ended === null ? null : yield* endUnlessEnrolling(token, request.checkinKey, ended);
  if (stopped !== null) return stopped;
  if (!newToToken && existing.rejectedAt !== null) {
    const open = yield* openAttemptFor(existing.id);
    // Reject spares a join Cloud may have sent, so the host keeps enrolling.
    const spared = open !== null && (joinCommitted(open, existing) || (yield* joinsSent(token.organizationId, [open.tokenId])).has(open.tokenId));
    if (!spared) return stop("rejected");
  }
  if (takingLastSlots && (yield* heldByToken(token.id)) >= WAITING_PER_TOKEN) {
    return yield* new CheckinRefused({
      status: 429,
      message: `This Fleet Token already has ${WAITING_PER_TOKEN} waiting servers.`,
    });
  }
  const prepared = request.prep === undefined ? request.report : { ...request.report, prep: request.prep };
  const reported: StoredReport = { ...prepared, cliVersion: request.cliVersion };
  const report: StoredReport = request.machinePhase === undefined ? reported : { ...reported, machinePhase: request.machinePhase };
  const fresh = { report, observedIp, probe: probeAfterCheckIn(observedIp, request.probe), lastCheckInAt: sql`now()` };
  // The capacity decision above holds only while the row is as it was read, so a changed row goes back through it.
  const [row] = newToToken
    ? yield* drizzle
      .insert(waitingServer)
      .values({ organizationId: token.organizationId, fleetTokenId: token.id, checkinKey: request.checkinKey, report, observedIp })
      .onConflictDoUpdate({
        target: [waitingServer.organizationId, waitingServer.checkinKey],
        set: { ...fresh, fleetTokenId: token.id, rejectedAt: null },
        setWhere: existing === undefined ? sql`false` : eq(waitingServer.fleetTokenId, existing.fleetTokenId),
      })
      .returning({ id: waitingServer.id, probe: waitingServer.probe })
    : yield* drizzle
      .update(waitingServer)
      .set(fresh)
      .where(and(key, eq(waitingServer.fleetTokenId, token.id), existing.rejectedAt === null ? isNull(waitingServer.rejectedAt) : undefined))
      .returning({ id: waitingServer.id, probe: waitingServer.probe });
  if (row === undefined) return OWNERSHIP_CHANGED;
  const answer = yield* answerAttempt(row.id, request, report);
  if (answer.kind !== "wait") return answer;
  // Handed out only with a wait, so an Enroll answer doesn't stamp a handout the host never sees.
  const probe = yield* handOutProbe(row.id, decodeStoredProbe(row.probe), observedIp, request.checkinKey);
  return probe === null ? answer : { ...answer, probe };
});

const WAIT: CheckinResponse = { kind: "wait", retryAfter: CHECKIN_WAIT_SECONDS };

const hasOpenEnrollment = Effect.fn("Checkin.hasOpenEnrollment")(function* (
  token: { readonly id: string; readonly organizationId: string },
  checkinKey: string,
) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ id: serverEnrollment.id })
    .from(serverEnrollment)
    .innerJoin(waitingServer, eq(waitingServer.id, serverEnrollment.waitingServerId))
    .where(and(
      eq(waitingServer.organizationId, token.organizationId),
      eq(waitingServer.checkinKey, checkinKey),
      eq(waitingServer.fleetTokenId, token.id),
      isNull(serverEnrollment.endedAt),
    ))
    .limit(1);
  return row !== undefined;
});

function reportOf(open: OpenAttempt, request: CheckinRequest): ServerEnrollmentCheckinReport | null {
  if (open.instruction.storage.kind === "zfs" && request.prep?.zfs === "failed" && !PAST_STORAGE.has(open.stage)) {
    return { attemptId: open.id, stage: "storage", failure: { stage: "storage", message: ZFS_BUILD_FAILED } };
  }
  const attempt = request.attempt;
  if (attempt?.attemptId !== open.id) return null;
  if (attempt.failure !== undefined) return { attemptId: open.id, stage: attempt.stage, failure: attempt.failure };
  return attempt.stage === open.stage ? null : { attemptId: open.id, stage: attempt.stage };
}

const answerAttempt = Effect.fn("Checkin.answerAttempt")(function* (
  waitingServerId: string,
  request: CheckinRequest,
  report: StoredReport,
) {
  const open = yield* openAttemptFor(waitingServerId);
  if (open === null) return WAIT;
  const committed = request.attempt?.attemptId === open.id && request.attempt.failure === undefined ? asStage(request.attempt.stage) : null;
  if (PAST_STORAGE.has(committed)) {
    // Reject spares a join it reads as committed. It locks this row first, so it never reads a report behind the host's.
    const { drizzle } = yield* Database;
    yield* drizzle.update(waitingServer).set({
      reportedAttemptId: open.id,
      reportedStage: forwardTo(waitingServer.reportedStage, committed, eq(waitingServer.reportedAttemptId, open.id)),
    }).where(eq(waitingServer.id, waitingServerId));
  }
  const reported = reportOf(open, request);
  if (reported !== null) {
    yield* afterDatabaseCommit(sendInngestEvent(createServerEnrollmentReportedEvent(reported)).pipe(
      Effect.catch((error) => Effect.logWarning("An enrollment report was not sent; the next check-in sends it again.", error)),
    ));
    if (reported.failure !== undefined) return WAIT;
  }
  const ready = open.state === "running" || (open.state === "instructed" && isReadyFor(report, open.instruction.storage));
  if (!ready || open.encryptedToken === null) return WAIT;
  const encryption = yield* SecretEncryption;
  return {
    kind: "enroll",
    attemptId: open.id,
    token: encryption.decrypt(open.encryptedToken),
    role: open.role,
    ...open.instruction,
  } satisfies CheckinResponse;
});

export const checkIn = Effect.fn("Checkin.checkIn")(function* (input: {
  readonly headers: Headers;
  readonly body: Uint8Array;
  readonly observedIp: string;
}) {
  const bearer = BEARER.exec(input.headers.get("authorization") ?? "")?.[1];
  const stamp = input.headers.get("ployz-timestamp") ?? "";
  const signature = SIGNATURE.exec(input.headers.get("ployz-signature") ?? "")?.[1];
  if (bearer === undefined || !TIMESTAMP.test(stamp) || signature === undefined) return yield* new Unauthorized();
  const timestamp = BigInt(stamp);
  const skew = BigInt(Math.floor((yield* Clock.currentTimeMillis) / 1000)) - timestamp;
  if (skew > MAX_SKEW_SECONDS || -skew > MAX_SKEW_SECONDS) return yield* new Unauthorized();

  const database = yield* Database;
  const [token] = yield* database.drizzle
    .select({
      id: fleetToken.id,
      organizationId: fleetToken.organizationId,
      autoEnrollMax: fleetToken.autoEnrollMax,
      revoked: sql<boolean>`${fleetToken.revokedAt} is not null`,
      expired: sql<boolean>`${fleetToken.expiresAt} <= now()`,
    })
    .from(fleetToken)
    .where(eq(fleetToken.tokenHash, hashFleetToken(bearer)))
    .limit(1);
  if (token === undefined) return yield* new Unauthorized();

  const claim = claimedKey(input.body);
  if (Option.isNone(claim)) return yield* new Unauthorized();
  const claimed = claim.value;
  const message = checkinMessage(claimed.checkinKey, timestamp, input.body);
  if (!verifyCheckin(claimed.checkinKey, message, Buffer.from(signature, "base64url"))) return yield* new Unauthorized();
  const parsed = decodeRequest(claimed.json);
  if (Option.isNone(parsed)) return yield* new CheckinRefused({ status: 400, message: "Invalid check-in." });
  const request = parsed.value;

  const ended = endedBy(token);
  const stopped = ended === null ? null : yield* database.transaction(endUnlessEnrolling(token, request.checkinKey, ended));
  if (stopped !== null) return stopped;

  // Every check-in of a token shares this row, so a fresh stamp is left alone and the update locks nothing.
  if (ended === null) {
    yield* database.drizzle
      .update(fleetToken)
      .set({ lastUsedAt: sql`now()` })
      .where(and(
        eq(fleetToken.id, token.id),
        or(isNull(fleetToken.lastUsedAt), lt(fleetToken.lastUsedAt, sql`now() - interval '1 minute'`)),
      ));
  }

  yield* sometimesPruneStaleRateLimits;
  const tooMany = new CheckinRefused({ status: 429, message: "Too many check-ins." });
  if (!(yield* countWithinRateLimit(`token:${token.id}`, PER_TOKEN_PER_SECOND))) return yield* tooMany;
  if (!(yield* countWithinRateLimit(`ip:${input.observedIp}`, PER_IP_PER_SECOND))) return yield* tooMany;

  for (let attempt = 0; attempt < 2; attempt++) {
    const answer = yield* database.transaction(admit(token, request, input.observedIp));
    if (answer === OWNERSHIP_CHANGED) continue;
    if (answer.kind === "wait" && token.autoEnrollMax !== null) {
      // The host is already admitted, so a failed start costs only this round; the next check-in tries again.
      const started = yield* startArrivalEnrollment(
        { id: token.id, organizationId: token.organizationId, autoEnrollMax: token.autoEnrollMax },
        request.checkinKey,
      ).pipe(Effect.catch((error) => Effect.logWarning("An arrival enroll did not start.", error).pipe(Effect.as(false))));
      if (started) return { ...answer, retryAfter: 2 };
      // Enroll on arrival never founds, so the host says it waits on the Organization's first server.
      if (!(yield* hasFirstServer(token.organizationId))) return { ...answer, noCluster: true } satisfies CheckinResponse;
    }
    return answer;
  }
  return yield* new CheckinRefused({ status: 429, message: "Busy, retry." });
});
