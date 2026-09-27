import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import type { DatabaseService } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { environmentDeployment } from "#/modules/deployments/tables";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/runtime-contract";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { hasUndeployedChanges } from "#/modules/environment-design/environment-change-set";
import { loadCurrentEnvironmentSnapshotProjection } from "#/modules/environment-design/working-state-repository.server";

/**
 * Merge and Update move only what a Branch runs: its Environment Change Set is empty and no attempt is active (queued
 * included). Holds the Branch's queue lock to commit, so no deploy is admitted meanwhile.
 */
export const assertBranchSettled = Effect.fn("Branches.assertBranchSettled")(function* (
  tx: DatabaseService["drizzle"], branchEnvironmentId: string,
) {
  yield* lockEnvironmentDeploymentQueue(branchEnvironmentId);
  const [active] = yield* tx.select({ id: environmentDeployment.id }).from(environmentDeployment).where(and(
    eq(environmentDeployment.environmentId, branchEnvironmentId),
    inArray(environmentDeployment.status, [...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES]),
  )).limit(1);
  if (active) return yield* new Conflict({ message: "A deploy of this branch is still running. Wait for it to finish." });
  const working = yield* loadCurrentEnvironmentSnapshotProjection(branchEnvironmentId);
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: branchEnvironmentId });
  const applied = projection.explicitStates.find((state) => state.environmentId === branchEnvironmentId)?.applied.nodes ?? [];
  if (hasUndeployedChanges(working.nodeSnapshots, applied)) {
    return yield* new Conflict({ message: "This branch has changes that aren't deployed. Deploy or discard them first." });
  }
});
