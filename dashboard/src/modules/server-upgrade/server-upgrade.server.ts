import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import type { MachineUpgradeAttempt, MachineUpgradeAttemptId } from "@ployz/sdk";
import { and, desc, eq, isNotNull, lt, max, sql, type SQL } from "drizzle-orm";
import { Data, Effect } from "effect";
import { PostHog } from "#/modules/analytics/posthog.server";
import type { Actor } from "#/modules/identity/actor";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createServerUpgradeRequestedEvent, type ServerUpgradeRequestedEventData } from "#/modules/inngest/events";
import { serverStatus, sortServers } from "#/modules/machines/server-status";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { rpcErrorCode } from "#/modules/runtime/ployz.server";
import {
  DEFAULT_SERVER_UPGRADE_SETTINGS,
  endEvidence,
  type FinalOutcome,
  isBehind,
  type LatestUpgrade,
  outlivedObservation,
  type ReleaseChannel,
  releaseFromPointer,
  releaseLine,
  type RequestServerUpgradeInput,
  type SetServerUpgradeSettingsInput,
  UPGRADE_OBSERVATION_LIMIT_MS,
  type UpgradeTrigger,
} from "#/modules/server-upgrade/server-upgrade";
import { organizationServerUpgrades, serverUpgradeAttempt } from "#/modules/server-upgrade/tables";
import { Database } from "#/server/database.server";

/** The daemon reads its Release Channel pointers here (core `CHANNEL_URL`). */
const CHANNEL_URL = "https://ployz.sh";
const POINTER_CACHE_MS = 5 * 60_000;
// ponytail: one process-wide cache of a public, tiny pointer per path; a handful of paths, nothing to evict.
const pointers = new Map<string, { readonly release: string | null; readonly readAt: number }>();

/** The Release Channel pointer could not be read; nothing is cached, so the next read tries again. */
export class ReleasePointerUnreadable extends Data.TaggedError("ReleasePointerUnreadable")<{
  readonly path: string;
  readonly cause: unknown;
}> {}

export class ServerUpgradeUnreachable extends Data.TaggedError("ServerUpgradeUnreachable")<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

/**
 * The newest release on Release Channel `channel` for release line `line` (`v0`), the pointer the daemon reads; with
 * no line, the unscoped pointer the installer reads, which may name a newer line. Null when the line has no pointer
 * or it names no published version; fails when it can't be read. A read is cached for a few minutes, a failure never.
 */
export const channelRelease = Effect.fn("ServerUpgrade.channelRelease")(function* (channel: ReleaseChannel, line: string | null) {
  const path = line === null ? channel : `${line}/${channel}`;
  const cached = pointers.get(path);
  if (cached !== undefined && Date.now() - cached.readAt < POINTER_CACHE_MS) return cached.release;
  const release = yield* Effect.tryPromise({
    try: async (signal) => {
      const response = await fetch(`${CHANNEL_URL}/${path}`, { signal: AbortSignal.any([signal, AbortSignal.timeout(5_000)]) });
      if (response.status === 404) return null;
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return releaseFromPointer(await response.text());
    },
    catch: (cause) => new ReleasePointerUnreadable({ path, cause }),
  });
  pointers.set(path, { release, readAt: Date.now() });
  return release;
});

/** The Release Channel the Organization chose; no settings row reads as the default. */
export const organizationReleaseChannel = Effect.fn("ServerUpgrade.channel")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ channel: organizationServerUpgrades.channel }).from(organizationServerUpgrades)
    .where(eq(organizationServerUpgrades.organizationId, organizationId));
  return row?.channel ?? DEFAULT_SERVER_UPGRADE_SETTINGS.channel;
});

/**
 * Each Server's latest Upgrade attempt in the Organization, keyed by Machine ID, and when the latest successful attempt
 * ended.
 */
export const listLatestServerUpgrades = Effect.fn("ServerUpgrade.listLatest")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const { drizzle } = yield* Database;
  const inOrganization = eq(serverUpgradeAttempt.organizationId, organization.id);
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
    .where(inOrganization)
    .orderBy(serverUpgradeAttempt.machineId, desc(serverUpgradeAttempt.startedAt));
  const [succeeded] = yield* drizzle.select({ endedAt: max(serverUpgradeAttempt.endedAt) }).from(serverUpgradeAttempt)
    .where(and(inOrganization, eq(serverUpgradeAttempt.outcome, "succeeded")));
  return {
    servers: Object.fromEntries(rows.map(({ machineId, startedAt, ...latest }) =>
      [machineId, { ...latest, startedAt: startedAt.toISOString() } satisfies LatestUpgrade])),
    lastUpgradedAt: succeeded?.endedAt?.toISOString() ?? null,
  };
});

/** Upgrade and Try again on a Server page, Upgrade on the Servers page: any member may. The rollout run records each attempt. */
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

/** Every Server the Runtime Watch observes online and idle, the only ones that can take an Upgrade, with its version. */
const observeOnlineServers = Effect.fn("ServerUpgrade.observeOnline")(function* (organizationId: string) {
  const session = yield* openSession(organizationId);
  const frame = yield* session.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS);
  return frame.machines.flatMap(({ machine, membership }) =>
    serverStatus({ membership, runningBuilds: machine.runtime.running_builds }) === "online"
      ? [{ id: machine.id, name: machine.name, version: machine.runtime.daemon_version }]
      : []);
}, Effect.scoped);

/** The version an online, idle Server runs; null when it isn't observed online and idle (it can't take an Upgrade). */
export const observeUpgradeableServer = Effect.fn("ServerUpgrade.observe")(function* (organizationId: string, machineId: string) {
  const online = yield* observeOnlineServers(organizationId);
  return online.find(({ id }) => id === machineId)?.version ?? null;
});

/**
 * The Organization's Halted releases: those whose latest attempt didn't succeed. A newer release has no
 * attempts, and a manual Upgrade of the release that succeeds becomes its latest, so either lifts the halt.
 */
const haltedReleases = Effect.fn("ServerUpgrade.halted")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const latest = yield* drizzle.selectDistinctOn([serverUpgradeAttempt.targetVersion], {
    target: serverUpgradeAttempt.targetVersion,
    outcome: serverUpgradeAttempt.outcome,
  }).from(serverUpgradeAttempt)
    .where(and(eq(serverUpgradeAttempt.organizationId, organizationId), isNotNull(serverUpgradeAttempt.targetVersion)))
    .orderBy(serverUpgradeAttempt.targetVersion, desc(serverUpgradeAttempt.startedAt));
  return new Set(latest.flatMap(({ target, outcome }) => target !== null && outcome !== "succeeded" ? [target] : []));
});

/**
 * Every Server observed online and idle on a release older than the newest on `channel` for its line, in name order:
 * a Server ahead of it (on a beta, after switching to Stable) isn't behind. An automatic rollout also skips a release
 * the Organization is halted on. A Cluster Cloud can't reach has none.
 */
export const listServersBehind = Effect.fn("ServerUpgrade.listBehind")(function* (
  organizationId: string,
  trigger: UpgradeTrigger,
  channel: ReleaseChannel,
) {
  const online = yield* observeOnlineServers(organizationId);
  const halted = trigger === "automatic" ? yield* haltedReleases(organizationId) : new Set<string>();
  const behind: Array<{ readonly id: string; readonly name: string; readonly status: "online" }> = [];
  for (const server of online) {
    const line = releaseLine(server.version);
    const release = line === null ? null : yield* channelRelease(channel, line);
    if (release !== null && isBehind(server.version, release) && !halted.has(release)) behind.push({ ...server, status: "online" });
  }
  return sortServers(behind).map(({ id }) => id);
}, Effect.catchIf(
  (error) => error instanceof ServerUpgradeUnreachable || error instanceof ReleasePointerUnreadable,
  (error) => Effect.logInfo("The Cluster or a Release Channel pointer can't be read; no Server is upgraded.", error)
    .pipe(Effect.as<string[]>([])),
));

/**
 * Write the attempt's row before its request. Its target starts as the release `channel` names for the Server's line,
 * so an attempt whose request never gets an answer still halts that release; the Server's answer replaces it with
 * the exact version it resolved.
 */
export const recordUpgradeAttempt = Effect.fn("ServerUpgrade.record")(function* (input: {
  readonly request: ServerUpgradeRequestedEventData & { readonly machineId: string };
  readonly attemptId: MachineUpgradeAttemptId;
  readonly channel: ReleaseChannel;
  readonly fromVersion: string;
  readonly inngestRunId: string;
}) {
  const line = releaseLine(input.fromVersion);
  // An unreadable pointer leaves the target to the Server's answer.
  const expected = line === null ? null : yield* channelRelease(input.channel, line).pipe(Effect.orElseSucceed(() => null));
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
    targetVersion: expected,
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
  readonly attemptId: MachineUpgradeAttemptId;
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

type AttemptOnServer = {
  readonly organizationId: string;
  readonly machineId: string;
  readonly attemptId: MachineUpgradeAttemptId;
};

/** The attempt as the Server reports it now; null while it can't be read (its daemon restarts mid-Upgrade). */
const inspectUpgradeOnServer = Effect.fn("ServerUpgrade.inspectOnServer")(function* (input: AttemptOnServer) {
  const session = yield* openSession(input.organizationId);
  const attempt = yield* session.inspectMachineUpgrade(input.machineId, input.attemptId);
  yield* noteAttempt(input.organizationId, attempt);
  return attempt;
}, Effect.scoped, Effect.catch((error) =>
  Effect.logInfo("The Upgrade attempt could not be read; polling again.", error).pipe(Effect.as(null))));

/**
 * One poll of a running attempt: `expired` once its row's `startedAt` is past the observation limit, as the hourly
 * sweep and the UI measure it, so a late outcome is not recorded over the unknown they already show; otherwise the
 * attempt as the Server reports it now, null while it can't be read.
 */
export const pollUpgradeOnServer = Effect.fn("ServerUpgrade.poll")(function* (input: AttemptOnServer) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ startedAt: serverUpgradeAttempt.startedAt }).from(serverUpgradeAttempt)
    .where(attemptWhere(input.organizationId, input.attemptId));
  if (row === undefined || outlivedObservation(row.startedAt, Date.now())) return "expired" as const;
  return yield* inspectUpgradeOnServer(input);
});

/** The outcome to record for a terminal attempt; null while it still runs. */
export function finalOutcome(attempt: MachineUpgradeAttempt): { outcome: FinalOutcome; stage: string | null; error: string | null } | null {
  if (attempt.outcome !== "succeeded" && attempt.outcome !== "failed" && attempt.outcome !== "interrupted") return null;
  const stage = "stage" in attempt ? attempt.stage : null;
  const error = "error" in attempt ? attempt.error : null;
  return { outcome: attempt.outcome, stage: null, error: null, ...endEvidence(attempt.outcome, stage, error) };
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
  const properties = { ...base, ...endEvidence(input.outcome, row.stage, row.error) };
  const posthog = yield* PostHog;
  // Automatic attempts belong to the Organization's system person; so does a manual one whose user was deleted.
  const userId = row.requestedByUserId ?? `system:${row.organizationId}`;
  if (row.requestedByUserId === null) {
    const [named] = yield* drizzle.select({ name: organization.name }).from(organization).where(eq(organization.id, row.organizationId));
    yield* posthog.identify(userId, { name: `${named?.name ?? row.organizationId} (system)`, is_system: true });
  }
  yield* posthog.capture({
    userId,
    event: `server_upgrade_${input.outcome}`,
    organizationId: row.organizationId,
    properties,
  });
  return true;
});

/** Close every attempt matching `where` that is still `running` as `unknown`; how many this call closed. */
const closeRunningAttempts = Effect.fn("ServerUpgrade.closeRunning")(function* (where: SQL | undefined) {
  const { drizzle } = yield* Database;
  const running = yield* drizzle.select({ organizationId: serverUpgradeAttempt.organizationId, attemptId: serverUpgradeAttempt.attemptId })
    .from(serverUpgradeAttempt)
    .where(and(eq(serverUpgradeAttempt.outcome, "running"), where));
  const closed = yield* Effect.forEach(running, (row) => finishUpgradeAttempt({ ...row, outcome: "unknown", stage: null, error: null }));
  return closed.filter(Boolean).length;
});

/** A Rollout run that failed or was cancelled lost track of its attempts: each still `running` reads `unknown`. */
export const closeRunUpgradeAttempts = (inngestRunId: string) =>
  closeRunningAttempts(eq(serverUpgradeAttempt.inngestRunId, inngestRunId));

/** The hourly sweep: an attempt still `running` after the observation limit lost its Rollout run, so it reads `unknown`. */
export const closeStaleUpgradeAttempts = () =>
  closeRunningAttempts(lt(serverUpgradeAttempt.startedAt, new Date(Date.now() - UPGRADE_OBSERVATION_LIMIT_MS)));

/** Every Organization with a founded Cluster and automatic upgrades on (no settings row reads as the default). */
export const listAutomaticUpgradeOrganizationIds = Effect.fn("ServerUpgrade.listAutomatic")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ id: organizationPairing.organizationId }).from(organizationPairing)
    .leftJoin(organizationServerUpgrades, eq(organizationServerUpgrades.organizationId, organizationPairing.organizationId))
    .where(and(
      isNotNull(organizationPairing.founderMachineId),
      sql`coalesce(${organizationServerUpgrades.automatic}, ${DEFAULT_SERVER_UPGRADE_SETTINGS.automatic})`,
    ));
  return rows.map(({ id }) => id);
});

/** "Upgrade automatically" and "Releases" in the Server upgrades dialog: any member may change them. */
export const setServerUpgradeSettings = Effect.fn("ServerUpgrade.setSettings")(function* (
  actor: Actor,
  { organizationSlug, ...settings }: SetServerUpgradeSettingsInput,
) {
  const { id: organizationId } = yield* requireInfrastructureOrganization(actor, organizationSlug);
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.insert(organizationServerUpgrades).values({ organizationId, ...settings })
    .onConflictDoUpdate({ target: organizationServerUpgrades.organizationId, set: { ...settings, updatedAt: new Date() } })
    .returning({
      id: organizationServerUpgrades.organizationId,
      automatic: organizationServerUpgrades.automatic,
      channel: organizationServerUpgrades.channel,
    });
  // ponytail: an upsert always returns its row; the fallback only satisfies the type.
  return row ?? { id: organizationId, ...DEFAULT_SERVER_UPGRADE_SETTINGS, ...settings };
});

/** Cloud mints attempt IDs in the daemon's 32-hex form. */
// SAFETY: a UUID without its dashes is 32 lowercase hex digits, the daemon's attempt ID form.
export const mintAttemptId = () => randomUUID().replaceAll("-", "") as MachineUpgradeAttemptId;
