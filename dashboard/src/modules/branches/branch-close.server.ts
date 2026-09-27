import "@tanstack/react-start/server-only";

import { and, eq, inArray, isNotNull, max } from "drizzle-orm";
import { Cause, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { dueForIdleClose } from "#/modules/branches/idle-close";
import { environmentBranch as schemaEnvironmentBranch, project as schemaProject } from "#/modules/project/tables";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import type { BranchCloseReason } from "#/modules/runtime/teardown";
import { confirmSystemTeardown } from "#/modules/runtime/teardown.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

/**
 * The system closes a Branch (after a Merge, or when it sits idle) through the Environment teardown, which takes its
 * own Branches first. It runs as the Branch's creator; a person closes one through the teardown's typed confirmation.
 */
export const closeBranch = Effect.fn("Branches.close")(function* (input: {
  readonly environmentId: string;
  readonly reason: Exclude<BranchCloseReason, "manual">;
}) {
  const database = yield* Database;
  const [branch] = yield* database.drizzle
    .select({ createdByUserId: schemaEnvironmentBranch.createdByUserId })
    .from(schemaEnvironmentBranch)
    .where(eq(schemaEnvironmentBranch.environmentId, input.environmentId));
  if (branch === undefined) return yield* new NotFound({ message: "The branch was not found." });
  return yield* confirmSystemTeardown({
    environmentId: input.environmentId,
    requestedByUserId: branch.createdByUserId,
    closeReason: input.reason,
  });
});

/** A kept Branch stays after merging and never closes for being idle. */
export const setBranchKept = Effect.fn("Branches.setKept")(function* (actor: Actor, input: {
  readonly organizationSlug: string;
  readonly environmentId: string;
  readonly kept: boolean;
}) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const database = yield* Database;
  const [row] = yield* database.drizzle
    .update(schemaEnvironmentBranch)
    .set({ kept: input.kept })
    .where(and(
      eq(schemaEnvironmentBranch.environmentId, input.environmentId),
      eq(schemaEnvironmentBranch.organizationId, organization.id),
    ))
    .returning();
  if (row === undefined) return yield* new NotFound({ message: "The branch was not found." });
  return { ...row, base: withoutSealedCiphertext(row.base) };
});

/**
 * Closes every idle Branch, across organizations, as the system. A close that fails (an unreachable runtime, a
 * teardown already running) leaves that Branch for the next sweep and never stops the others.
 */
export const sweepIdleBranches = Effect.fn("Branches.sweepIdle")(function* (now: Date) {
  const database = yield* Database;
  const branches = yield* database.drizzle
    .select({
      environmentId: schemaEnvironmentBranch.environmentId,
      parentEnvironmentId: schemaEnvironmentBranch.parentEnvironmentId,
      kept: schemaEnvironmentBranch.kept,
    })
    .from(schemaEnvironmentBranch);
  if (branches.length === 0) return [];
  const branchIds = branches.map((branch) => branch.environmentId);
  const latest = yield* database.drizzle
    .select({ environmentId: schemaEnvironmentDeployment.environmentId, at: max(schemaEnvironmentDeployment.createdAt) })
    .from(schemaEnvironmentDeployment)
    .where(inArray(schemaEnvironmentDeployment.environmentId, branchIds))
    .groupBy(schemaEnvironmentDeployment.environmentId);
  const defaults = yield* database.drizzle
    .select({ environmentId: schemaProject.defaultEnvironmentId })
    .from(schemaProject)
    .where(and(isNotNull(schemaProject.defaultEnvironmentId), inArray(schemaProject.defaultEnvironmentId, branchIds)));
  const due = dueForIdleClose({
    branches,
    latestAttemptAt: new Map(latest.flatMap((row) => (row.at === null ? [] : [[row.environmentId, row.at]]))),
    defaultEnvironmentIds: new Set(defaults.flatMap((row) => (row.environmentId === null ? [] : [row.environmentId]))),
  }, now);
  const closed = yield* Effect.forEach(due, (environmentId) => closeBranch({ environmentId, reason: "idle" }).pipe(
    Effect.scoped,
    Effect.as([environmentId]),
    Effect.catchCause((cause) => Cause.hasInterrupts(cause)
      ? Effect.failCause(cause)
      : Effect.logWarning("An idle Branch did not close; the next sweep retries it.", { environmentId, cause }).pipe(Effect.as([]))),
  ));
  return closed.flat();
});
