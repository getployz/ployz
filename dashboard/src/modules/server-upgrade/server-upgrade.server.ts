import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import type { MachineUpgradeAttempt } from "@ployz/sdk";
import { and, desc, eq, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { PostHog } from "#/modules/analytics/posthog.server";
import type { Actor } from "#/modules/identity/actor";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createServerUpgradeRequestedEvent, type ServerUpgradeRequestedEventData } from "#/modules/inngest/events";
import { serverStatus } from "#/modules/machines/server-status";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { rpcErrorCode } from "#/modules/runtime/ployz.server";
import {
  type LatestUpgrade,
  type ReleaseChannel,
  releaseFromPointer,
  type RequestServerUpgradeInput,
  type UpgradeOutcome,
} from "#/modules/server-upgrade/server-upgrade";
import { serverUpgradeAttempt } from "#/modules/server-upgrade/tables";
import { Database } from "#/server/database.server";

/** The daemon reads its Release Channel pointers here (core `CHANNEL_URL`). */
const CHANNEL_URL = "https://ployz.sh";
const POINTER_CACHE_MS = 5 * 60_000;
// ponytail: one process-wide cache of a public, tiny pointer per release line; nothing to evict.
const pointers = new Map<string, { readonly release: string | null; readonly readAt: number }>();

export class ServerUpgradeUnreachable extends Data.TaggedError("ServerUpgradeUnreachable")<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

/**
 * The newest release on the `stable` Release Channel for release line `line` (`v0`), the pointer the daemon reads;
 * null when it can't be read. Cached for a few minutes.
 */
export const stableRelease = Effect.fn("ServerUpgrade.stableRelease")(function* (line: string) {
  const cached = pointers.get(line);
  if (cached !== undefined && Date.now() - cached.readAt < POINTER_CACHE_MS) return cached.release;
  const release = yield* Effect.tryPromise(async (signal) => {
    const response = await fetch(`${CHANNEL_URL}/${line}/stable`, { signal: AbortSignal.any([signal, AbortSignal.timeout(5_000)]) });
    return response.ok ? releaseFromPointer(await response.text()) : null;
  }).pipe(Effect.catch((cause) => Effect.logWarning("The stable Release Channel pointer could not be read.", cause).pipe(Effect.as(null))));
  pointers.set(line, { release, readAt: Date.now() });
  return release;
});

/** Each Server's latest Upgrade attempt in the Organization, keyed by Machine ID. */
export const listLatestServerUpgrades = Effect.fn("ServerUpgrade.listLatest")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinctOn([serverUpgradeAttempt.machineId], {
    machineId: serverUpgradeAttempt.machineId,
    attemptId: serverUpgradeAttempt.attemptId,
    outcome: serverUpgradeAttempt.outcome,
    stage: serverUpgradeAttempt.stage,
    error: serverUpgradeAttempt.error,
    fromVersion: serverUpgradeAttempt.fromVersion,
    targetVersion: serverUpgradeAttempt.targetVersion,
    startedAt: serverUpgradeAttempt.startedAt,
  }).from(serverUpgradeAttempt)
    .where(eq(serverUpgradeAttempt.organizationId, organization.id))
    .orderBy(serverUpgradeAttempt.machineId, desc(serverUpgradeAttempt.startedAt));
  return Object.fromEntries(rows.map(({ machineId, startedAt, ...latest }) =>
    [machineId, { ...latest, startedAt: startedAt.toISOString() } satisfies LatestUpgrade]));
});

/** Upgrade and Try again on a Server page: any member may. The rollout run records the attempt. */
export const requestServerUpgrade = Effect.fn("ServerUpgrade.request")(function* (
  actor: Actor,
  input: RequestServerUpgradeInput,
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  yield* sendInngestEvent(createServerUpgradeRequestedEvent({
    organizationId: organization.id,
    machineId: input.machineId,
    trigger: "manual",
    userId: actor.userId,
  }));
});

const openSession = Effect.fn("ServerUpgrade.openSession")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") {
    return yield* new ServerUpgradeUnreachable({ operation: "open organization runtime", cause: session.status });
  }
  return session.connected;
});

/** The version an online, idle Server runs; null when it isn't observed online and idle (it can't take an Upgrade). */
export const observeUpgradeableServer = Effect.fn("ServerUpgrade.observe")(function* (organizationId: string, machineId: string) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS);
  const observed = frame.machines.find(({ machine }) => machine.id === machineId);
  if (observed === undefined) return null;
  const status = serverStatus({ membership: observed.membership, runningBuilds: observed.machine.runtime.running_builds });
  return status === "online" ? observed.machine.runtime.daemon_version : null;
}, Effect.scoped);

export const recordUpgradeAttempt = Effect.fn("ServerUpgrade.record")(function* (input: {
  readonly request: ServerUpgradeRequestedEventData;
  readonly attemptId: string;
  readonly channel: ReleaseChannel;
  readonly fromVersion: string;
  readonly inngestRunId: string;
}) {
  const { drizzle } = yield* Database;
  // A retried step finds the row it already wrote.
  yield* drizzle.insert(serverUpgradeAttempt).values({
    organizationId: input.request.organizationId,
    machineId: input.request.machineId,
    attemptId: input.attemptId,
    trigger: input.request.trigger,
    requestedByUserId: input.request.userId,
    channel: input.channel,
    fromVersion: input.fromVersion,
    inngestRunId: input.inngestRunId,
    startedAt: new Date(),
  }).onConflictDoNothing();
});

const attemptWhere = (organizationId: string, attemptId: string) =>
  and(eq(serverUpgradeAttempt.organizationId, organizationId), eq(serverUpgradeAttempt.attemptId, attemptId));

/** Keep the exact target and the latest stage the Server reported on the row. */
const noteAttempt = Effect.fn("ServerUpgrade.note")(function* (organizationId: string, attempt: MachineUpgradeAttempt) {
  const { drizzle } = yield* Database;
  const stage = "stage" in attempt ? attempt.stage : undefined;
  yield* drizzle.update(serverUpgradeAttempt).set({ targetVersion: attempt.target, stage }).where(and(attemptWhere(organizationId, attempt.attempt_id), eq(serverUpgradeAttempt.outcome, "running")));
});

/**
 * Ask the Server to Upgrade along `channel`, never to an exact version. Busy (a build or another installation just
 * started) drops the row: a refused request is no attempt.
 */
export const requestUpgradeOnServer = Effect.fn("ServerUpgrade.requestOnServer")(function* (input: {
  readonly organizationId: string;
  readonly machineId: string;
  readonly attemptId: string;
  readonly channel: ReleaseChannel;
}) {
  const session = yield* openSession(input.organizationId);
  const attempt = yield* session.requestMachineUpgrade(input.machineId, input.attemptId, input.channel).pipe(
    Effect.map((attempt) => ({ busy: false as const, attempt })),
    Effect.catchIf((error) => rpcErrorCode(error) === "conflict", () => Effect.succeed({ busy: true as const })),
    Effect.mapError((cause) => new ServerUpgradeUnreachable({ operation: "request machine upgrade", cause })),
  );
  if (attempt.busy) {
    const { drizzle } = yield* Database;
    yield* drizzle.delete(serverUpgradeAttempt).where(attemptWhere(input.organizationId, input.attemptId));
    return null;
  }
  yield* noteAttempt(input.organizationId, attempt.attempt);
  return attempt.attempt;
}, Effect.scoped);

/** The attempt as the Server reports it now; null while it can't be read (its daemon restarts mid-Upgrade). */
export const inspectUpgradeOnServer = Effect.fn("ServerUpgrade.inspectOnServer")(function* (input: {
  readonly organizationId: string;
  readonly machineId: string;
  readonly attemptId: string;
}) {
  const session = yield* openSession(input.organizationId);
  const attempt = yield* session.inspectMachineUpgrade(input.machineId, input.attemptId);
  yield* noteAttempt(input.organizationId, attempt);
  return attempt;
}, Effect.scoped, Effect.catch((error) =>
  Effect.logInfo("The Upgrade attempt could not be read; polling again.", error).pipe(Effect.as(null))));

export type FinalOutcome = Exclude<UpgradeOutcome, "running">;

/** The outcome to record for a terminal attempt; null while it still runs. */
export function finalOutcome(attempt: MachineUpgradeAttempt): { outcome: FinalOutcome; stage: string | null; error: string | null } | null {
  switch (attempt.outcome) {
    case "succeeded":
      return { outcome: "succeeded", stage: null, error: null };
    case "failed":
      return { outcome: "failed", stage: attempt.stage, error: attempt.error };
    case "interrupted":
      return { outcome: "interrupted", stage: attempt.stage, error: null };
    default:
      return null;
  }
}

/**
 * Record an attempt's outcome and send its one PostHog event. Only the write that ends a `running` row sends it, so
 * a retried step or a second closer never sends twice. Unknown keeps the last stage seen.
 */
export const finishUpgradeAttempt = Effect.fn("ServerUpgrade.finish")(function* (input: {
  readonly organizationId: string;
  readonly attemptId: string;
  readonly outcome: FinalOutcome;
  readonly stage: string | null;
  readonly error: string | null;
}) {
  const { drizzle } = yield* Database;
  const endedAt = new Date();
  const [row] = yield* drizzle.update(serverUpgradeAttempt).set({
    outcome: input.outcome,
    stage: input.outcome === "succeeded" ? null : sql`coalesce(${input.stage}, ${serverUpgradeAttempt.stage})`,
    error: input.error,
    endedAt,
  }).where(and(attemptWhere(input.organizationId, input.attemptId), eq(serverUpgradeAttempt.outcome, "running"))).returning();
  if (row === undefined) return false;
  const base = {
    trigger: row.trigger,
    channel: row.channel,
    from_version: row.fromVersion,
    to_version: row.targetVersion,
    total_seconds: Math.round((endedAt.getTime() - row.startedAt.getTime()) / 1000),
  };
  // A non-success says where it stopped; a failure also says why.
  const properties = input.outcome === "succeeded" ? base
    : input.outcome === "failed" ? { ...base, stage: row.stage, error: row.error }
    : { ...base, stage: row.stage };
  const posthog = yield* PostHog;
  yield* posthog.capture({
    userId: row.requestedByUserId ?? `system:${row.organizationId}`,
    event: `server_upgrade_${input.outcome}`,
    organizationId: row.organizationId,
    properties,
  });
  return true;
});

/** A rollout run that failed or was cancelled lost track of its attempts: each still `running` reads `unknown`. */
export const closeRunUpgradeAttempts = Effect.fn("ServerUpgrade.closeRun")(function* (inngestRunId: string) {
  const { drizzle } = yield* Database;
  const running = yield* drizzle.select({ organizationId: serverUpgradeAttempt.organizationId, attemptId: serverUpgradeAttempt.attemptId })
    .from(serverUpgradeAttempt)
    .where(and(eq(serverUpgradeAttempt.inngestRunId, inngestRunId), eq(serverUpgradeAttempt.outcome, "running")));
  yield* Effect.forEach(running, (row) => finishUpgradeAttempt({ ...row, outcome: "unknown", stage: null, error: null }));
  return running.length;
});

/** Cloud mints attempt IDs in the daemon's 32-hex form. */
export const mintAttemptId = () => randomUUID().replaceAll("-", "");
