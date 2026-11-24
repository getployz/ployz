import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import { Clock, Effect, Option, Schema } from "effect";
import { sql } from "drizzle-orm";
import type { MachineId } from "@ployz/sdk";
import { rustMachineIdSchema } from "#/lib/schema";
import { MAX_SKEW_SECONDS, PUSH_DOMAIN, PUSH_SIGNATURE, PUSH_TIMESTAMP } from "#/modules/observe-ingest/machine-wire";
import { organizationOfSigningMachine, verifyManagementSignature } from "#/modules/observe-ingest/machine-signers.server";
import { observeIngestCursor } from "#/modules/observe-ingest/tables";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";

export type ObservePusher = { readonly organizationId: string; readonly machineId: MachineId };

/** What a push's path and headers claim before its body is read: the Machine, when it signed, and the signature. */
export type ObservePushCredentials = { readonly machineId: MachineId; readonly timestamp: bigint; readonly signature: Uint8Array };

const MAX_SKEW = BigInt(MAX_SKEW_SECONDS);

/** The bytes a Machine signs: domain, Machine id, timestamp (i64 big-endian unix seconds), and the body's sha256. */
export function observePushMessage(machineId: string, timestamp: bigint, body: Uint8Array) {
  const stamp = Buffer.alloc(8);
  stamp.writeBigInt64BE(timestamp);
  return Buffer.concat([PUSH_DOMAIN,Buffer.from(machineId, "ascii"), Buffer.from([0]), stamp, createHash("sha256").update(body).digest()]);
}

const decodeMachineId = Schema.decodeUnknownOption(rustMachineIdSchema);

/** Refuses a push whose headers are missing, malformed or signed outside the clock window, before its body is read. */
export const observePushCredentials = Effect.fn("ObserveIngest.credentials")(function* (machineId: string, headers: Headers) {
  const stamp = headers.get("ployz-timestamp") ?? "";
  const signature = PUSH_SIGNATURE.exec(headers.get("ployz-signature") ?? "")?.[1];
  const machine = decodeMachineId(machineId);
  if (!PUSH_TIMESTAMP.test(stamp) || signature === undefined || Option.isNone(machine)) return yield* new Unauthorized();
  const timestamp = BigInt(stamp);
  const now = BigInt(Math.floor((yield* Clock.currentTimeMillis) / 1000));
  const skew = now - timestamp;
  if (skew > MAX_SKEW || -skew > MAX_SKEW) return yield* new Unauthorized();
  return { machineId: machine.value, timestamp, signature: Buffer.from(signature, "base64url") } satisfies ObservePushCredentials;
});

/** The one Organization whose enrolled Machine signed this body. */
export const authenticateObservePush = Effect.fn("ObserveIngest.authenticate")(function* (
  { machineId, timestamp, signature }: ObservePushCredentials,
  body: Uint8Array,
) {
  const message = observePushMessage(machineId, timestamp, body);
  const organizationId = yield* organizationOfSigningMachine(machineId, (identity) => verifyManagementSignature(identity, message, signature));
  return { organizationId, machineId } satisfies ObservePusher;
});

/** A signed salt fetch is contact from the Machine, which the Servers-offline warning reads from the cursors. */
export const recordSaltFetch = Effect.fn("ObserveIngest.saltFetch")(function* (pusher: ObservePusher) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(observeIngestCursor)
    .values({ organizationId: pusher.organizationId, machineId: pusher.machineId, stream: "salts", seq: 0n })
    .onConflictDoUpdate({
      target: [observeIngestCursor.organizationId, observeIngestCursor.machineId, observeIngestCursor.stream],
      set: { updatedAt: sql`now()` },
    });
});
