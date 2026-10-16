import "@tanstack/react-start/server-only";
import { createHash, createPublicKey, verify } from "node:crypto";
import { and, count, eq, sql } from "drizzle-orm";
import { Clock, Data, Effect, Option, Schema } from "effect";
import {
  CHECKIN_WAIT_SECONDS,
  CheckinRequest,
  type CheckinResponse,
  type CheckinStopReason,
} from "#/modules/machines/waiting/checkin-wire";
import { hashFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { takeRateLimitSlot } from "#/modules/machines/waiting/rate-limit.server";
import { fleetToken, waitingServer } from "#/modules/machines/waiting/tables";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";

export const WAITING_PER_TOKEN = 1000;
const EXCLUSIVE_NEAR_CAP = 100;
const PER_TOKEN_PER_SECOND = 300;
const PER_IP_PER_SECOND = 50;

const MAX_SKEW_SECONDS = 300n;
const TIMESTAMP = /^(0|[1-9]\d{0,11})$/;
const SIGNATURE = /^v1=([A-Za-z0-9_-]{86})$/;
const BEARER = /^Bearer (pflt_\S+)$/u;
const DOMAIN = Buffer.from("ployz-checkin-v1\0", "ascii");

/** A check-in Cloud won't take, with the HTTP status the host sees. */
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

function parseRequest(body: Uint8Array) {
  try {
    return decodeRequest(JSON.parse(new TextDecoder().decode(body)));
  } catch {
    return Option.none();
  }
}

const heldByToken = Effect.fn("Checkin.heldByToken")(function* (fleetTokenId: string) {
  const { drizzle } = yield* Database;
  const [held] = yield* drizzle
    .select({ rows: count() })
    .from(waitingServer)
    .where(eq(waitingServer.fleetTokenId, fleetTokenId));
  return held?.rows ?? 0;
});

/**
 * The only writer of `waiting_server`. Authenticates a host's signed check-in by its Fleet Token and Check-in Key,
 * records what it reports, and answers wait or stop.
 */
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
    .select({ id: fleetToken.id, organizationId: fleetToken.organizationId })
    .from(fleetToken)
    .where(eq(fleetToken.tokenHash, hashFleetToken(bearer)))
    .limit(1);
  if (token === undefined) return yield* new Unauthorized();

  const parsed = parseRequest(input.body);
  if (Option.isNone(parsed)) return yield* new CheckinRefused({ status: 400, message: "Invalid check-in." });
  const request = parsed.value;
  const message = checkinMessage(request.checkinKey, timestamp, input.body);
  if (!verifyCheckin(request.checkinKey, message, Buffer.from(signature, "base64url"))) {
    return yield* new Unauthorized();
  }

  const withinToken = yield* takeRateLimitSlot(`token:${token.id}`, PER_TOKEN_PER_SECOND);
  const withinIp = yield* takeRateLimitSlot(`ip:${input.observedIp}`, PER_IP_PER_SECOND);
  if (!withinToken || !withinIp) return yield* new CheckinRefused({ status: 429, message: "Too many check-ins." });

  return yield* database.transaction(
    Effect.gen(function* () {
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
      // Every check-in shares the token row lock, so a concurrent revoke waits and then deletes the row it wrote. Only a
      // new key near the cap locks it exclusively and recounts, so two hosts can't both take the last slot; far from the
      // cap, a fleet's first check-ins don't queue behind each other.
      const nearCap = heldBefore >= WAITING_PER_TOKEN - EXCLUSIVE_NEAR_CAP;
      const [live] = yield* drizzle
        .select({
          revoked: sql<boolean>`${fleetToken.revokedAt} is not null`,
          expired: sql<boolean>`${fleetToken.expiresAt} <= now()`,
        })
        .from(fleetToken)
        .where(eq(fleetToken.id, token.id))
        .for(nearCap ? "update" : "share");
      const ended = live === undefined || live.revoked ? "token_revoked" : live.expired ? "token_expired" : null;
      if (ended !== null) {
        yield* drizzle.delete(waitingServer).where(and(key, eq(waitingServer.fleetTokenId, token.id)));
        return stop(ended);
      }
      if (!newToToken && existing.rejectedAt !== null) return stop("rejected");
      if (nearCap && (yield* heldByToken(token.id)) >= WAITING_PER_TOKEN) {
        return yield* new CheckinRefused({
          status: 429,
          message: `This Fleet Token already has ${WAITING_PER_TOKEN} waiting servers.`,
        });
      }
      const report = request.prep === undefined ? request.report : { ...request.report, prep: request.prep };
      const [row] = yield* drizzle
        .insert(waitingServer)
        .values({
          organizationId: token.organizationId,
          fleetTokenId: token.id,
          checkinKey: request.checkinKey,
          report,
          observedIp: input.observedIp,
          probe: request.probe ?? null,
        })
        .onConflictDoUpdate({
          target: [waitingServer.organizationId, waitingServer.checkinKey],
          set: {
            fleetTokenId: token.id,
            report,
            observedIp: input.observedIp,
            probe: sql`coalesce(excluded.probe, ${waitingServer.probe})`,
            lastCheckInAt: sql`now()`,
            rejectedAt: null,
          },
          setWhere: sql`${waitingServer.rejectedAt} is null or ${waitingServer.fleetTokenId} <> excluded.fleet_token_id`,
        })
        .returning({ id: waitingServer.id });
      if (row === undefined) return stop("rejected");
      return { kind: "wait", retryAfter: CHECKIN_WAIT_SECONDS } satisfies CheckinResponse;
    }),
  );
});
