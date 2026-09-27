import "@tanstack/react-start/server-only";

import { and, eq, inArray, isNotNull, max, or } from "drizzle-orm";
import { Cause, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { dueForIdleClose } from "#/modules/branches/idle-close";
import { environmentBranch as schemaEnvironmentBranch, project as schemaProject } from "#/modules/project/tables";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { teardownAttempt as schemaTeardownAttempt } from "#/modules/runtime/tables";
import { confirmSystemTeardown } from "#/modules/runtime/teardown.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import type { SetBranchKept } from "./branch-schemas";

/**
 * The system closes a Branch (after a Merge, or when it sits idle) through the Environment teardown, which takes its
 * own Branches first. It runs as the Branch's creator; a person closes one through the teardown's typed confirmation.
 */
export const closeBranch = Effect.fn("Branches.close")(function* (environmentId: string) {
  const database = yield* Database;
  const [branch] = yield* database.drizzle
    .select({ organizationId: schemaEnvironmentBranch.organizationId, createdByUserId: schemaEnvironmentBranch.createdByUserId })
    .from(schemaEnvironmentBranch)
    .where(eq(schemaEnvironmentBranch.environmentId, environmentId));
  if (branch === undefined) return yield* new NotFound({ message: "The branch was not found." });
  return yield* confirmSystemTeardown({
    organizationId: branch.organizationId,
    environmentId,
    requestedByUserId: branch.createdByUserId,
  });
});

/** Runs a system close: true once its teardown started, else false with `why` logged. Only an interruption fails it. */
const tryClose = <E, R>(environmentId: string, why: string, close: Effect.Effect<boolean, E, R>) => close.pipe(
  Effect.scoped,
  Effect.catchCause((cause) => Cause.hasInterrupts(cause)
    ? Effect.failCause(cause)
    : Effect.logWarning(why, { environmentId, cause }).pipe(Effect.as(false))),
);

/** Closes a merged Branch; false when it couldn't, and the Merge stands. */
export const tryCloseBranch = (environmentId: string) =>
  tryClose(environmentId, "A merged Branch did not close.", closeBranch(environmentId).pipe(Effect.as(true)));

/** A kept Branch stays after merging and never closes for being idle. */
export const setBranchKept = Effect.fn("Branches.setKept")(function* (actor: Actor, input: SetBranchKept) {
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
 * Closes every idle Branch, across organizations, as the system. Each close checks the rule again and admits the teardown
 * in one transaction, holding the Branch's queue lock (a deploy waits) and its row (Keep and a new Branch of it wait).
 * A close that fails (an unreachable runtime) leaves that Branch for the next sweep and never stops the others.
 */
export const sweepIdleBranches = Effect.fn("Branches.sweepIdle")(function* (now: Date) {
  const database = yield* Database;
  const closed = yield* Effect.forEach(yield* idleBranches(now), (environmentId) => tryClose(
    environmentId,
    "An idle Branch did not close; the next sweep retries it.",
    database.transaction(Effect.gen(function* () {
      yield* lockEnvironmentDeploymentQueue(environmentId);
      yield* database.drizzle.select({ id: schemaEnvironmentBranch.environmentId }).from(schemaEnvironmentBranch)
        .where(eq(schemaEnvironmentBranch.environmentId, environmentId)).for("update");
      if (!(yield* idleBranches(now, environmentId)).includes(environmentId)) return false;
      yield* closeBranch(environmentId);
      return true;
    })),
  ).pipe(Effect.map((done) => (done ? [environmentId] : []))));
  return closed.flat();
});

/**
 * The Branches due to close for sitting idle, skipping any whose teardown is already running. With `only`, just that
 * Branch and its children are read.
 */
const idleBranches = Effect.fn("Branches.idleBranches")(function* (now: Date, only?: string) {
  const { drizzle } = yield* Database;
  const branches = yield* drizzle
    .select({
      environmentId: schemaEnvironmentBranch.environmentId,
      parentEnvironmentId: schemaEnvironmentBranch.parentEnvironmentId,
      kept: schemaEnvironmentBranch.kept,
    })
    .from(schemaEnvironmentBranch)
    .where(only === undefined ? undefined
      : or(eq(schemaEnvironmentBranch.environmentId, only), eq(schemaEnvironmentBranch.parentEnvironmentId, only)));
  if (branches.length === 0) return [];
  const branchIds = branches.map((branch) => branch.environmentId);
  const latest = yield* drizzle
    .select({ environmentId: schemaEnvironmentDeployment.environmentId, at: max(schemaEnvironmentDeployment.createdAt) })
    .from(schemaEnvironmentDeployment)
    .where(inArray(schemaEnvironmentDeployment.environmentId, branchIds))
    .groupBy(schemaEnvironmentDeployment.environmentId);
  const defaults = yield* drizzle
    .select({ environmentId: schemaProject.defaultEnvironmentId })
    .from(schemaProject)
    .where(and(isNotNull(schemaProject.defaultEnvironmentId), inArray(schemaProject.defaultEnvironmentId, branchIds)));
  const closing = new Set((yield* drizzle
    .select({ environmentId: schemaTeardownAttempt.environmentId })
    .from(schemaTeardownAttempt)
    .where(and(
      inArray(schemaTeardownAttempt.environmentId, branchIds),
      inArray(schemaTeardownAttempt.status, ["pending", "running"]),
    ))).map((row) => row.environmentId));
  return dueForIdleClose({
    branches,
    latestAttemptAt: new Map(latest.flatMap((row) => (row.at === null ? [] : [[row.environmentId, row.at]]))),
    defaultEnvironmentIds: new Set(defaults.flatMap((row) => (row.environmentId === null ? [] : [row.environmentId]))),
  }, now).filter((environmentId) => !closing.has(environmentId));
});
