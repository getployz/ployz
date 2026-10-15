import "@tanstack/react-start/server-only";
import { createHash, createPublicKey, verify } from "node:crypto";
import { and, eq, isNull, sql } from "drizzle-orm";
import { Clock, Effect, Option, Schema } from "effect";
import type { MachineId } from "#/db/tables";
import { createObserveCrashReceivedEvent } from "#/modules/inngest/events";
import { decryptPairingSecret } from "#/modules/machines/connections.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { managementIdentity } from "#/modules/machines/management-identity";
import { organizationMachine } from "#/modules/machines/tables";
import type { CrashPushInput } from "#/modules/observe-ingest/ingest";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";

export type ObservePusher = { readonly organizationId: string; readonly machineId: MachineId };

/** What a push's path and headers claim before its body is read: the Machine, when it signed, and the signature. */
export type ObservePushCredentials = { readonly machineId: MachineId; readonly timestamp: bigint; readonly signature: Uint8Array };

const MAX_SKEW_SECONDS = 300n;
const TIMESTAMP = /^(0|[1-9]\d{0,11})$/;
const SIGNATURE = /^v1=([A-Za-z0-9_-]{86})$/;
const DOMAIN = Buffer.from("ployz-push-v1\0", "ascii");

/** The bytes a Machine signs: domain, Machine id, timestamp (i64 big-endian unix seconds), and the body's sha256. */
export function observePushMessage(machineId: string, timestamp: bigint, body: Uint8Array) {
  const stamp = Buffer.alloc(8);
  stamp.writeBigInt64BE(timestamp);
  return Buffer.concat([DOMAIN, Buffer.from(machineId, "ascii"), Buffer.from([0]), stamp, createHash("sha256").update(body).digest()]);
}

/** Whether `signature` is the ed25519 signature of `message` by the management key `identity` (64 hex digits). */
export function verifyObservePush(identity: string, message: Uint8Array, signature: Uint8Array) {
  try {
    const x = Buffer.from(identity, "hex").toString("base64url");
    return verify(null, message, createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x }, format: "jwk" }), signature);
  } catch {
    return false;
  }
}

const decodeMachineId = Schema.decodeUnknownOption(rustMachineIdSchema);

/** Refuses a push whose headers are missing, malformed or signed outside the clock window, before its body is read. */
export const observePushCredentials = Effect.fn("ObserveIngest.credentials")(function* (machineId: string, headers: Headers) {
  const stamp = headers.get("ployz-timestamp") ?? "";
  const signature = SIGNATURE.exec(headers.get("ployz-signature") ?? "")?.[1];
  const machine = decodeMachineId(machineId);
  if (!TIMESTAMP.test(stamp) || signature === undefined || Option.isNone(machine)) return yield* new Unauthorized();
  const timestamp = BigInt(stamp);
  const now = BigInt(Math.floor((yield* Clock.currentTimeMillis) / 1000));
  const skew = now - timestamp;
  if (skew > MAX_SKEW_SECONDS || -skew > MAX_SKEW_SECONDS) return yield* new Unauthorized();
  return { machineId: machine.value, timestamp, signature: Buffer.from(signature, "base64url") } satisfies ObservePushCredentials;
});

/**
 * The one Organization whose enrolled Machine signed this body. A row enrolled before Cloud kept management keys gets
 * its key from its capability on first use.
 */
export const authenticateObservePush = Effect.fn("ObserveIngest.authenticate")(function* (
  { machineId, timestamp, signature }: ObservePushCredentials,
  body: Uint8Array,
) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({
    organizationId: organizationMachine.organizationId,
    managementIdentity: organizationMachine.managementIdentity,
    encryptedCapability: organizationMachine.encryptedCapability,
  }).from(organizationMachine).where(eq(organizationMachine.machineId, machineId));
  const message = observePushMessage(machineId, timestamp, body);
  const signers: string[] = [];
  for (const row of rows) {
    const identity = row.managementIdentity ?? managementIdentity(yield* decryptPairingSecret(row.encryptedCapability));
    if (identity === null) continue;
    if (row.managementIdentity === null) {
      yield* drizzle.update(organizationMachine).set({ managementIdentity: identity }).where(and(
        eq(organizationMachine.organizationId, row.organizationId),
        eq(organizationMachine.machineId, machineId),
        isNull(organizationMachine.managementIdentity),
      ));
    }
    if (verifyObservePush(identity, message, signature)) signers.push(row.organizationId);
  }
  const [organizationId, ...others] = signers;
  if (organizationId === undefined) return yield* new Unauthorized();
  if (others.length > 0) {
    // A Machine that left one Organization's Cluster without Cloud seeing it keeps its key, and its row, there.
    yield* Effect.logWarning("A push verified for more than one Organization; refused.", { machineId, organizations: signers.length });
    return yield* new Unauthorized();
  }
  return { organizationId, machineId } satisfies ObservePusher;
});

export const ingestCrashPush = Effect.fn("ObserveIngest.crash")(function* (pusher: ObservePusher, push: CrashPushInput) {
  if (push.dropped > 0) {
    yield* Effect.logWarning("A Machine dropped crash events it could not keep for Cloud.", { ...pusher, dropped: push.dropped });
  }
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.insert(observeIngestCursor)
      .values({ organizationId: pusher.organizationId, machineId: pusher.machineId, stream: "crash", seq: push.seq })
      .onConflictDoUpdate({
        target: [observeIngestCursor.organizationId, observeIngestCursor.machineId, observeIngestCursor.stream],
        set: { seq: sql`greatest(${observeIngestCursor.seq}, excluded.seq)`, updatedAt: new Date() },
      });
    if (push.events.length === 0) return;
    const kept = yield* drizzle.insert(crashEvent)
      .values(push.events.map((event) => ({ ...event, organizationId: pusher.organizationId, machineId: pusher.machineId })))
      .onConflictDoNothing()
      .returning({ id: crashEvent.id, serviceId: crashEvent.serviceId });
    const firstByService = new Map<string, string>();
    for (const row of kept) if (!firstByService.has(row.serviceId)) firstByService.set(row.serviceId, row.id);
    for (const [serviceId, crashEventId] of firstByService) {
      yield* enqueueEvent(createObserveCrashReceivedEvent({ organizationId: pusher.organizationId, serviceId }, crashEventId));
    }
  }));
});
