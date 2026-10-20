import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { connectionsOf } from "#/modules/config-store/config-store.server";
import { type CleanupLeft, deploymentCleanup, type DeploymentCleanupState } from "#/modules/config-store/tables";
import type { ConfigDeploymentRetireRequestedEventData } from "#/modules/inngest/events";
import { Database } from "#/server/database.server";

/**
 * Retire the old copies the Deployment's run replaced, all inside the Rust SDK: Cloud names the Deployment and the Store
 * knows which copies those are. Resolves to the copies still running.
 */
export const retireOldCopies = Effect.fn("DeploymentCleanup.retireOld")(function* (data: ConfigDeploymentRetireRequestedEventData) {
  const store = yield* cloudStore;
  const connections = yield* connectionsOf(data.organizationId);
  return yield* storeTry<{ left: CleanupLeft[] }>(() => store.retireOld(data.organizationId, data.deploymentId, connections));
});

/** Run `runId` cleans up the Deployment, `left` still running so far. A newer run of the same Deployment takes the row. */
export const recordCleanupWaiting = Effect.fn("DeploymentCleanup.waiting")(function* (
  runId: string, data: ConfigDeploymentRetireRequestedEventData, left: readonly CleanupLeft[] = [],
) {
  const { drizzle } = yield* Database;
  const waiting = { runId, state: "waiting" as const, left: [...left], message: null, updatedAt: new Date() };
  yield* drizzle.insert(deploymentCleanup)
    .values({ deploymentId: data.deploymentId, organizationId: data.organizationId, environmentId: data.environmentId, ...waiting })
    .onConflictDoUpdate({ target: deploymentCleanup.deploymentId, set: waiting });
});

export type CleanupEnd = { state: Exclude<DeploymentCleanupState, "waiting">; left?: readonly CleanupLeft[]; message?: string };

/** Run `runId`'s cleanup ended. Only a waiting row ends, so a late failure or cancel never rewrites how it ended. */
export const endCleanup = Effect.fn("DeploymentCleanup.end")(function* (runId: string, end: CleanupEnd) {
  const { drizzle } = yield* Database;
  const ended = yield* drizzle.update(deploymentCleanup)
    .set({ state: end.state, left: [...end.left ?? []], message: end.message ?? null, updatedAt: new Date() })
    .where(and(eq(deploymentCleanup.runId, runId), eq(deploymentCleanup.state, "waiting")))
    .returning({ deploymentId: deploymentCleanup.deploymentId });
  return ended.length;
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
