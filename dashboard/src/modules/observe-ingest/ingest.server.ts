import "@tanstack/react-start/server-only";
import { createHash, createPublicKey, verify } from "node:crypto";
import { and, eq, inArray, isNull, sql } from "drizzle-orm";
import { Clock, Effect, Option, Schema } from "effect";
import type { MachineId } from "#/db/tables";
import { createObserveCrashReceivedEvent } from "#/modules/inngest/events";
import { decryptPairingSecret } from "#/modules/machines/connections.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { managementIdentity } from "#/modules/machines/management-identity";
import { organizationMachine } from "#/modules/machines/tables";
import type { BotsInput, CrashPushInput, GapInput, PageviewInput, PageviewPushInput } from "#/modules/observe-ingest/ingest";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { ENTRY_LOOKBACK_SECONDS, localDays, retentionCutoff, SESSION_GAP_SECONDS } from "#/modules/web-analytics/local-time";
import type { Site } from "#/modules/web-analytics/model";
import { ClickHouse } from "#/modules/web-analytics/clickhouse/client";
import { enqueueForClickHouse } from "#/modules/web-analytics/clickhouse/writer";
import { ensurePartitions, monthOf } from "#/modules/web-analytics/postgres/partitions";
import { namespacesByEnvironment, siteDay } from "#/modules/web-analytics/salts.server";
import { analyticsBotDaily, analyticsGapDaily, analyticsPageview, analyticsRollupState, analyticsSite } from "#/modules/web-analytics/tables";
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

const DAY_MS = 86_400_000;
const NO_BATCH_YET = -1n;

type IngestSite = Pick<Site, "environmentId" | "timezone">;

const rollupDaysChangedBy = (timezone: string, ts: Date) =>
  localDays(timezone, new Date(ts.getTime() - SESSION_GAP_SECONDS * 1000), new Date(ts.getTime() + ENTRY_LOOKBACK_SECONDS * 1000 + 1));

export const ingestPageviewPush = Effect.fn("ObserveIngest.pageviews")(function* (pusher: ObservePusher, push: PageviewPushInput) {
  if (push.dropped > 0) {
    yield* Effect.logWarning("A Machine dropped pageviews it could not keep for Cloud.", { ...pusher, dropped: push.dropped });
  }
  if (!(yield* AppConfig).analyticsEnabled) return;
  const { drizzle } = yield* Database;
  const enabled = yield* drizzle.select({ environmentId: analyticsSite.environmentId, timezone: analyticsSite.timezone })
    .from(analyticsSite)
    .where(and(eq(analyticsSite.organizationId, pusher.organizationId), eq(analyticsSite.enabled, true)));
  if (enabled.length === 0) return;
  const namespaces = yield* namespacesByEnvironment(pusher.organizationId);
  const sites = new Map<string, IngestSite>();
  for (const site of enabled) {
    const namespace = namespaces.get(site.environmentId);
    if (namespace !== undefined) sites.set(namespace, site);
  }

  const now = yield* Clock.currentTimeMillis;
  const floor = retentionCutoff(now);
  const pageviews: Array<{ readonly site: IngestSite; readonly row: PageviewInput }> = [];
  const bots = new Map<string, Omit<BotsInput, "kind" | "namespace"> & { readonly environmentId: string }>();
  const gaps = new Map<string, Omit<GapInput, "kind" | "namespace"> & { readonly environmentId: string; readonly day: string }>();
  for (const event of push.events) {
    const site = sites.get(event.namespace);
    if (site === undefined) continue;
    if (event.kind === "pageview") {
      if (event.ts < floor || event.ts.getTime() > now + DAY_MS) continue;
      pageviews.push({ site, row: event });
    } else if (event.kind === "bots") {
      const key = JSON.stringify([site.environmentId, event.day, event.reason]);
      bots.set(key, { environmentId: site.environmentId, day: event.day, reason: event.reason, count: (bots.get(key)?.count ?? 0) + event.count });
    } else {
      const day = event.day ?? siteDay(site.timezone, now);
      const key = JSON.stringify([site.environmentId, day]);
      gaps.set(key, { environmentId: site.environmentId, day, count: (gaps.get(key)?.count ?? 0) + event.count });
    }
  }
  if (pageviews.length === 0 && bots.size === 0 && gaps.size === 0) return;
  yield* ensurePartitions(pageviews.map(({ row }) => monthOf(row.ts)));

  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const cursorKey = and(
      eq(observeIngestCursor.organizationId, pusher.organizationId),
      eq(observeIngestCursor.machineId, pusher.machineId),
      eq(observeIngestCursor.stream, "pageviews"),
    );
    yield* drizzle.insert(observeIngestCursor)
      .values({ organizationId: pusher.organizationId, machineId: pusher.machineId, stream: "pageviews", seq: NO_BATCH_YET })
      .onConflictDoNothing();
    const [cursor] = yield* drizzle.select({ seq: observeIngestCursor.seq }).from(observeIngestCursor).where(cursorKey).for("update");
    yield* drizzle.update(observeIngestCursor)
      .set({ seq: sql`greatest(${observeIngestCursor.seq}, ${push.seq.toString()}::bigint)`, updatedAt: new Date(now) })
      .where(cursorKey);

    // Every write below waits on its environments' rollup_state rows, locked in id order, the order the rollup and the
    // environment delete trigger take them, so a delete racing this batch waits for it or it for the delete.
    const environmentIds = [...new Set([
      ...pageviews.map(({ site }) => site.environmentId),
      ...[...bots.values()].map((row) => row.environmentId),
      ...[...gaps.values()].map((row) => row.environmentId),
    ])].sort();
    const rolledUpTo = new Map<string, string | null>();
    for (const environmentId of environmentIds) {
      yield* drizzle.insert(analyticsRollupState).values({ environmentId }).onConflictDoNothing();
      const [state] = yield* drizzle.select({ rolledUpTo: analyticsRollupState.rolledUpTo }).from(analyticsRollupState)
        .where(eq(analyticsRollupState.environmentId, environmentId))
        .for("update");
      rolledUpTo.set(environmentId, state?.rolledUpTo ?? null);
    }
    const sites = environmentIds.length === 0 ? [] : yield* drizzle
      .select({ environmentId: analyticsSite.environmentId, timezone: analyticsSite.timezone, enabled: analyticsSite.enabled })
      .from(analyticsSite)
      .where(inArray(analyticsSite.environmentId, environmentIds));
    const timezones = new Map(sites.filter((site) => site.enabled).map((site) => [site.environmentId, site.timezone]));
    // An environment deleted before this batch has no site: drop the state row the lock above just made for it.
    const gone = environmentIds.filter((id) => !sites.some((site) => site.environmentId === id));
    if (gone.length > 0) yield* drizzle.delete(analyticsRollupState).where(inArray(analyticsRollupState.environmentId, gone));

    if (push.seq > (cursor?.seq ?? NO_BATCH_YET)) {
      const keptBots = [...bots.values()].filter((row) => timezones.has(row.environmentId));
      if (keptBots.length > 0) {
        yield* drizzle.insert(analyticsBotDaily).values(keptBots).onConflictDoUpdate({
          target: [analyticsBotDaily.environmentId, analyticsBotDaily.day, analyticsBotDaily.reason],
          set: { count: sql`${analyticsBotDaily.count} + excluded.count` },
        });
      }
      const keptGaps = [...gaps.values()].filter((row) => timezones.has(row.environmentId));
      if (keptGaps.length > 0) {
        yield* drizzle.insert(analyticsGapDaily).values(keptGaps).onConflictDoUpdate({
          target: [analyticsGapDaily.environmentId, analyticsGapDaily.day],
          set: { count: sql`${analyticsGapDaily.count} + excluded.count` },
        });
      }
    }

    const admitted = pageviews.filter(({ site }) => timezones.has(site.environmentId));
    if (admitted.length === 0) return;

    const inserted = yield* drizzle.insert(analyticsPageview)
      .values(admitted.map(({ site, row: { kind: _kind, namespace: _namespace, ...row } }) => ({ ...row, environmentId: site.environmentId })))
      .onConflictDoNothing()
      .returning();
    if ((yield* ClickHouse) !== null) yield* enqueueForClickHouse(inserted);
    const dirty = new Map<string, Set<string>>();
    for (const row of inserted) {
      const timezone = timezones.get(row.environmentId);
      const closed = rolledUpTo.get(row.environmentId);
      if (timezone === undefined) throw new Error(`inserted a pageview for ${row.environmentId}, which has no enabled site`);
      if (closed == null) continue;
      const days = dirty.get(row.environmentId) ?? new Set<string>();
      for (const day of rollupDaysChangedBy(timezone, row.ts)) if (day <= closed) days.add(day);
      dirty.set(row.environmentId, days);
    }
    for (const [environmentId, days] of dirty) {
      if (days.size === 0) continue;
      yield* drizzle.update(analyticsRollupState)
        .set({
          dirty: sql`array(select distinct d from unnest(${analyticsRollupState.dirty} || ${sql.param([...days])}::date[]) d order by d)`,
          updatedAt: new Date(now),
        })
        .where(eq(analyticsRollupState.environmentId, environmentId));
    }
  }));
});
