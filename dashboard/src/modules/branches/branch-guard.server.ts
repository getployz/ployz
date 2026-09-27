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
 * Why a Branch runs something other than its Working State, or null: an active attempt (queued included), or a non-empty
 * Environment Change Set. Holds the Branch's queue lock to commit, so no deploy is admitted meanwhile.
 */
export const branchUnsettled = Effect.fn("Branches.branchUnsettled")(function* (
  tx: DatabaseService["drizzle"], branchEnvironmentId: string,
) {
  yield* lockEnvironmentDeploymentQueue(branchEnvironmentId);
  const [active] = yield* tx.select({ id: environmentDeployment.id }).from(environmentDeployment).where(and(
    eq(environmentDeployment.environmentId, branchEnvironmentId),
    inArray(environmentDeployment.status, [...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES]),
  )).limit(1);
  if (active) return "A deploy of this branch is still running. Wait for it to finish.";
  const working = yield* loadCurrentEnvironmentSnapshotProjection(branchEnvironmentId);
  const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: branchEnvironmentId });
  const applied = [...projection.appliedSavedNodeByKey.values()].filter((node) => node.environmentId === branchEnvironmentId);
  return hasUndeployedChanges(working.nodeSnapshots, applied)
    ? "This branch has changes that aren't deployed. Deploy or discard them first." : null;
});

/** Merge and Update move only what a Branch runs: refused unless it is settled (branchUnsettled). */
export const assertBranchSettled = Effect.fn("Branches.assertBranchSettled")(function* (
  tx: DatabaseService["drizzle"], branchEnvironmentId: string,
) {
  const unsettled = yield* branchUnsettled(tx, branchEnvironmentId);
  if (unsettled) return yield* new Conflict({ message: unsettled });
});
