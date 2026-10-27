import "@tanstack/react-start/server-only";
import { and, eq, lt, ne, or } from "drizzle-orm";
import { Effect, Option } from "effect";
import { cloudStore, refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
import { connectionsOf } from "#/modules/config-store/config-store.server";
import { type CleanupLeft, deploymentCleanup, type DeploymentCleanupState } from "#/modules/config-store/tables";
import type { InngestRunStatus } from "#/modules/inngest/client";
import type { ConfigDeploymentRetireRequestedEventData } from "#/modules/inngest/events";
import { Database } from "#/server/database.server";

type RetireOutcome = { left: CleanupLeft[]; kept: CleanupLeft[] };

/**
 * Retire the old copies the Deployment's run replaced, all inside the Rust SDK: Cloud names the Deployment and the Store
 * knows which copies those are. Resolves to the copies still running: `left` a retry may retire, `kept` it never will.
 * A Deployment the Store no longer has went with its Environment: that removal answers for what is left, not this cleanup.
 */
export const retireOldCopies = Effect.fn("DeploymentCleanup.retireOld")(function* (data: ConfigDeploymentRetireRequestedEventData) {
  const store = yield* cloudStore;
  const connections = yield* connectionsOf(data.organizationId);
  return yield* storeTry<RetireOutcome>(() => store.retireOld(data.organizationId, data.deploymentId, connections))
    .pipe(Effect.catchIf(refusedWith("not_found"), () => Effect.succeed<RetireOutcome>({ left: [], kept: [] })));
});

const cleanupOf = (data: ConfigDeploymentRetireRequestedEventData) =>
  ({ deploymentId: data.deploymentId, organizationId: data.organizationId, environmentId: data.environmentId });

const ownWaitingRow = (runId: string) => and(eq(deploymentCleanup.runId, runId), eq(deploymentCleanup.state, "waiting"));

/**
 * Run `runId` cleans up the Deployment, `left` still running so far. A newer run of the same Deployment takes the row,
 * but the run's own ended row stays ended: its cancel can land before this write does.
 */
export const recordCleanupWaiting = Effect.fn("DeploymentCleanup.waiting")(function* (
  runId: string, data: ConfigDeploymentRetireRequestedEventData, left: readonly CleanupLeft[] = [],
) {
  const { drizzle } = yield* Database;
  const waiting = { runId, state: "waiting" as const, left: [...left], message: null, updatedAt: new Date() };
  yield* drizzle.insert(deploymentCleanup)
    .values({ ...cleanupOf(data), ...waiting })
    .onConflictDoUpdate({
      target: deploymentCleanup.deploymentId,
      set: waiting,
      setWhere: or(ne(deploymentCleanup.runId, runId), eq(deploymentCleanup.state, "waiting")),
    });
});

export type CleanupEnd = { state: Exclude<DeploymentCleanupState, "waiting">; left?: readonly CleanupLeft[]; message?: string };

/**
 * Run `runId`'s cleanup ended. Given the Deployment it cleans up, the end lands even before the run's first waiting row.
 * It never rewrites how a run ended, nor takes the row from another run still waiting.
 */
export const endCleanup = Effect.fn("DeploymentCleanup.end")(function* (
  runId: string, end: CleanupEnd, requested: Option.Option<ConfigDeploymentRetireRequestedEventData>,
) {
  const { drizzle } = yield* Database;
  const outcome = { runId, state: end.state, message: end.message ?? null, updatedAt: new Date() };
  // A cancel or a failure that names no copies keeps the last ones the row named; Done leaves none.
  const keepsLast = (end.state === "cancelled" || end.state === "failed") && end.left === undefined;
  const ended = keepsLast ? outcome : { ...outcome, left: [...end.left ?? []] };
  const returning = { deploymentId: deploymentCleanup.deploymentId };
  const rows = Option.isNone(requested)
    ? yield* drizzle.update(deploymentCleanup).set(ended).where(ownWaitingRow(runId)).returning(returning)
    : yield* drizzle.insert(deploymentCleanup)
      .values({ ...cleanupOf(requested.value), ...ended })
      .onConflictDoUpdate({
        target: deploymentCleanup.deploymentId,
        set: ended,
        setWhere: or(ownWaitingRow(runId), and(ne(deploymentCleanup.runId, runId), ne(deploymentCleanup.state, "waiting"))),
      })
      .returning(returning);
  return rows.length;
});

/** Inngest can report a run that just started as missing, so only a row this quiet is worth asking about. */
const CLEANUP_QUIET_MS = 15 * 60_000;

const LOST_CLEANUP = "Cloud lost track of this cleanup. Deploy again to retire what it left.";

/**
 * End each waiting cleanup whose Inngest run is gone, failed with what it last left running. A run Inngest still reports
 * running, or that Inngest can't answer for, keeps its row.
 */
export const closeStaleCleanups = Effect.fn("DeploymentCleanup.closeStale")(function* <R>(
  runStatus: (runId: string) => Effect.Effect<InngestRunStatus, unknown, R>,
) {
  const { drizzle } = yield* Database;
  const quiet = yield* drizzle.select({ runId: deploymentCleanup.runId, left: deploymentCleanup.left }).from(deploymentCleanup)
    .where(and(eq(deploymentCleanup.state, "waiting"), lt(deploymentCleanup.updatedAt, new Date(Date.now() - CLEANUP_QUIET_MS))));
  let closed = 0;
  for (const { runId, left } of quiet) {
    const status = yield* runStatus(runId).pipe(Effect.option);
    if (Option.isNone(status) || status.value === "running") continue;
    closed += yield* endCleanup(runId, { state: "failed", left, message: LOST_CLEANUP }, Option.none());
  }
  return { closed };
});

/** The Deployment's cleanup, or null for one Cloud never cleaned up. */
export const readDeploymentCleanup = Effect.fn("DeploymentCleanup.read")(function* (organizationId: string, deploymentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ state: deploymentCleanup.state, left: deploymentCleanup.left, message: deploymentCleanup.message })
    .from(deploymentCleanup)
    .where(and(eq(deploymentCleanup.organizationId, organizationId), eq(deploymentCleanup.deploymentId, deploymentId)))
    .limit(1);
  return row ?? null;
});
