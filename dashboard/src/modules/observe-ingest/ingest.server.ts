import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import { and, eq, sql } from "drizzle-orm";
import { Clock, Effect, Option, Schema } from "effect";
import type { MachineId } from "#/db/tables";
import { createObserveCrashReceivedEvent } from "#/modules/inngest/events";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import type { CrashPushInput, RequestPushInput } from "#/modules/observe-ingest/ingest";
import { organizationOfSigningMachine, verifyManagementSignature } from "#/modules/observe-ingest/machine-signers.server";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { classify } from "#/modules/web-analytics/classify";
import { retentionCutoff } from "#/modules/web-analytics/local-time";
import { dayOf, ensurePartitions } from "#/modules/web-analytics/postgres/partitions";
import { namespacesByEnvironment } from "#/modules/web-analytics/salts.server";
import { analyticsRequest, analyticsSite } from "#/modules/web-analytics/tables";
import { AppConfig } from "#/server/config.server";
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
export const verifyObservePush = verifyManagementSignature;

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

/** The one Organization whose enrolled Machine signed this body. */
export const authenticateObservePush = Effect.fn("ObserveIngest.authenticate")(function* (
  { machineId, timestamp, signature }: ObservePushCredentials,
  body: Uint8Array,
) {
  const message = observePushMessage(machineId, timestamp, body);
  const organizationId = yield* organizationOfSigningMachine(machineId, (identity) => verifyObservePush(identity, message, signature));
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

const DAY_MS = 86_400_000;

/** The rows of a requests push that land on a site with analytics on, within retention. */
const analyticsRows = Effect.fn("ObserveIngest.analyticsRows")(function* (pusher: ObservePusher, push: RequestPushInput, now: number) {
  if (!(yield* AppConfig).analyticsEnabled) return [];
  const { drizzle } = yield* Database;
  const enabled = yield* drizzle.select({ environmentId: analyticsSite.environmentId })
    .from(analyticsSite)
    .where(and(eq(analyticsSite.organizationId, pusher.organizationId), eq(analyticsSite.enabled, true)));
  if (enabled.length === 0) return [];
  const namespaces = yield* namespacesByEnvironment(pusher.organizationId);
  const environments = new Map<string, string>();
  for (const site of enabled) {
    const namespace = namespaces.get(site.environmentId);
    if (namespace !== undefined) environments.set(namespace, site.environmentId);
  }
  const cutoff = retentionCutoff(now);
  return push.rows.flatMap(({ namespace, ua, contentType, secPurpose, ...row }) => {
    const environmentId = environments.get(namespace);
    if (environmentId === undefined || row.ts < cutoff || row.ts.getTime() > now + DAY_MS) return [];
    return [{ ...row, environmentId, ...classify({ method: row.method, status: row.status, ua, contentType, secPurpose }) }];
  });
});

/**
 * Keeps the push's analytics rows, and advances the cursor even when it keeps none: a Machine that serves traffic keeps
 * its salts fresh with requests pushes and sends no crash refresh, so this cursor is its only recent push time.
 */
export const ingestRequestPush = Effect.fn("ObserveIngest.requests")(function* (pusher: ObservePusher, push: RequestPushInput) {
  if (push.dropped > 0) {
    yield* Effect.logWarning("A Machine dropped requests it could not keep for Cloud.", { ...pusher, dropped: push.dropped });
  }
  const now = yield* Clock.currentTimeMillis;
  const rows = yield* analyticsRows(pusher, push, now);
  yield* ensurePartitions(rows.map((row) => dayOf(row.ts)));

  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.insert(observeIngestCursor)
      .values({ organizationId: pusher.organizationId, machineId: pusher.machineId, stream: "requests", seq: push.seq })
      .onConflictDoUpdate({
        target: [observeIngestCursor.organizationId, observeIngestCursor.machineId, observeIngestCursor.stream],
        set: { seq: sql`greatest(${observeIngestCursor.seq}, excluded.seq)`, updatedAt: new Date(now) },
      });
    if (rows.length > 0) yield* drizzle.insert(analyticsRequest).values(rows).onConflictDoNothing();
  }));
});

