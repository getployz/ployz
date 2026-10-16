import "@tanstack/react-start/server-only";
import { createHash, createPublicKey, verify } from "node:crypto";
import { and, count, eq, isNull, sql } from "drizzle-orm";
import { Clock, Data, Effect, Option, Schema } from "effect";
import {
  CHECKIN_WAIT_SECONDS,
  CheckinKey,
  CheckinRequest,
  type CheckinResponse,
  type CheckinStopReason,
  type StoredReport,
} from "#/modules/machines/waiting/checkin-wire";
import { createServerEnrollmentReportedEvent, type ServerEnrollmentReportedEventData } from "#/modules/inngest/events";
import { sendInngestEvent } from "#/modules/inngest/client";
import { openAttemptFor, type OpenAttempt } from "#/modules/machines/waiting/enrollment-run.server";
import { hashFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { countWithinRateLimit, sometimesPruneStaleRateLimits } from "#/modules/machines/waiting/rate-limit.server";
import { isReadyFor, ZFS_BUILD_FAILED } from "#/modules/machines/waiting/server-enrollment";
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

const endWaiting = Effect.fn("Checkin.endWaiting")(function* (
  token: { readonly id: string; readonly organizationId: string },
  checkinKey: string,
  reason: CheckinStopReason,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(waitingServer).where(and(
    eq(waitingServer.organizationId, token.organizationId),
    eq(waitingServer.checkinKey, checkinKey),
    eq(waitingServer.fleetTokenId, token.id),
  ));
  return stop(reason);
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
  const [existing] = yield* drizzle
    .select({ fleetTokenId: waitingServer.fleetTokenId, rejectedAt: waitingServer.rejectedAt })
    .from(waitingServer)
    .where(key);
  const newToToken = existing?.fleetTokenId !== token.id;
  const heldBefore = newToToken ? yield* heldByToken(token.id) : 0;
  const takingLastSlots = heldBefore >= WAITING_PER_TOKEN - LAST_SLOTS;
  const live = yield* lockTokenAgainstRevoke(token.id, takingLastSlots ? "update" : "share");
  const ended = endedBy(live);
  if (ended !== null && !(yield* enrolling(token, request.checkinKey))) return yield* endWaiting(token, request.checkinKey, ended);
  if (!newToToken && existing.rejectedAt !== null) return stop("rejected");
  if (takingLastSlots && (yield* heldByToken(token.id)) >= WAITING_PER_TOKEN) {
    return yield* new CheckinRefused({
      status: 429,
      message: `This Fleet Token already has ${WAITING_PER_TOKEN} waiting servers.`,
    });
  }
  const prepared = request.prep === undefined ? request.report : { ...request.report, prep: request.prep };
  const report: StoredReport = { ...prepared, cliVersion: request.cliVersion };
  const fresh = { report, observedIp, probe: request.probe ?? sql`${waitingServer.probe}`, lastCheckInAt: sql`now()` };
  // The capacity decision above holds only while the row is as it was read, so a changed row goes back through it.
  const [row] = newToToken
    ? yield* drizzle
      .insert(waitingServer)
      .values({ organizationId: token.organizationId, fleetTokenId: token.id, checkinKey: request.checkinKey, report, observedIp, probe: request.probe ?? null })
      .onConflictDoUpdate({
        target: [waitingServer.organizationId, waitingServer.checkinKey],
        set: { ...fresh, fleetTokenId: token.id, rejectedAt: null },
        setWhere: existing === undefined ? sql`false` : eq(waitingServer.fleetTokenId, existing.fleetTokenId),
      })
      .returning({ id: waitingServer.id })
    : yield* drizzle
      .update(waitingServer)
      .set(fresh)
      .where(and(key, eq(waitingServer.fleetTokenId, token.id), isNull(waitingServer.rejectedAt)))
      .returning({ id: waitingServer.id });
  if (row === undefined) return OWNERSHIP_CHANGED;
  return yield* answerAttempt(row.id, request, report);
});

const WAIT: CheckinResponse = { kind: "wait", retryAfter: CHECKIN_WAIT_SECONDS };

/** A key whose Fleet Token ended keeps checking in while its enrollment is open, so the attempt can finish. */
const enrolling = Effect.fn("Checkin.enrolling")(function* (
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

/** What the host's attempt changed, for its run to hear; a stage it already reported is not news. */
function reportOf(open: OpenAttempt, request: CheckinRequest): ServerEnrollmentReportedEventData | null {
  if (open.instruction.storage.kind === "zfs" && request.prep?.zfs === "failed") {
    return { attemptId: open.id, stage: "storage", failure: { stage: "storage", message: ZFS_BUILD_FAILED } };
  }
  const attempt = request.attempt;
  if (attempt?.attemptId !== open.id) return null;
  if (attempt.failure !== undefined) return { attemptId: open.id, stage: attempt.stage, failure: attempt.failure };
  return attempt.stage === open.stage ? null : { attemptId: open.id, stage: attempt.stage };
}

/** Cloud answers `enroll` once the host can carry its attempt's storage, and keeps answering it while the attempt runs. */
const answerAttempt = Effect.fn("Checkin.answerAttempt")(function* (
  waitingServerId: string,
  request: CheckinRequest,
  report: StoredReport,
) {
  const open = yield* openAttemptFor(waitingServerId);
  if (open === null) return WAIT;
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
  if (ended !== null && !(yield* enrolling(token, request.checkinKey))) return yield* endWaiting(token, request.checkinKey, ended);

  yield* sometimesPruneStaleRateLimits;
  const tooMany = new CheckinRefused({ status: 429, message: "Too many check-ins." });
  if (!(yield* countWithinRateLimit(`token:${token.id}`, PER_TOKEN_PER_SECOND))) return yield* tooMany;
  if (!(yield* countWithinRateLimit(`ip:${input.observedIp}`, PER_IP_PER_SECOND))) return yield* tooMany;

  for (let attempt = 0; attempt < 2; attempt++) {
    const answer = yield* database.transaction(admit(token, request, input.observedIp));
    if (answer !== OWNERSHIP_CHANGED) return answer;
  }
  return yield* new CheckinRefused({ status: 429, message: "Busy, retry." });
});
