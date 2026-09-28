import "@tanstack/react-start/server-only";
import { loadBranchRows } from "#/modules/pr-environments/pr-environment.repository.server";

import { and, eq, inArray, isNotNull, max, or } from "drizzle-orm";
import { Cause, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { lockEnvironmentDeploymentQueues } from "#/modules/deployments/queue-lock.server";
import { dueForIdleClose } from "#/modules/branches/idle-close";
import { environmentBranch as schemaEnvironmentBranch, project as schemaProject } from "#/modules/project/tables";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { activeTeardownFor } from "#/modules/runtime/teardown.repository";
import { lockBranchScope } from "#/modules/environment-design/workspace-repository.server";
import { cancelActiveDeployments } from "#/modules/deployments/deployment-command.server";
import { admitSystemTeardown, prepareSystemTeardown } from "#/modules/runtime/teardown.server";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import type { SetBranchKept } from "./branch-schemas";

/** Why the system closes a Branch; a person closes one through the teardown's typed confirmation. */
export type BranchCloseReason = "saved" | "idle" | "pull_request_closed";

/**
 * The system closes a Branch (deleted after a Save, or when it sits idle) through the Environment teardown, which takes its
 * own Branches first. It runs as the Branch's creator. This asks the runtime, holding nothing, and returns `admit`, which
 * writes the teardown attempt (inside the caller's transaction, under its locks) and logs the close with its reason once
 * that commits.
 */
export const closeBranch = Effect.fn("Branches.close")(function* (environmentId: string, reason: BranchCloseReason) {
  const database = yield* Database;
  const [branch] = yield* database.drizzle
    .select({
      organizationId: schemaEnvironmentBranch.organizationId, projectId: schemaEnvironmentBranch.projectId,
      createdByUserId: schemaEnvironmentBranch.createdByUserId,
    })
    .from(schemaEnvironmentBranch)
    .where(eq(schemaEnvironmentBranch.environmentId, environmentId));
  if (branch === undefined) return yield* new NotFound({ message: "The branch was not found." });
  const requestedByUserId = branch.createdByUserId;
  const prepared = yield* prepareSystemTeardown({ organizationId: branch.organizationId, environmentId });
  return {
    projectId: branch.projectId,
    /** The Environments the teardown removes (the Branch and its Branches): their queues come before any document. */
    environmentIds: prepared.expected,
    admit: Effect.gen(function* () {
      const attempt = yield* admitSystemTeardown(prepared, requestedByUserId);
      yield* afterDatabaseCommit(Effect.logInfo("A Branch is closing.", { environmentId, reason, requestedByUserId, teardownAttemptId: attempt.id }));
      return attempt;
    }),
  };
});

/** Runs a system close: true once its teardown started, else false with `why` logged. Only an interruption fails it. */
const tryClose = <E, R>(environmentId: string, why: string, close: Effect.Effect<{ id: string } | null, E, R>) => close.pipe(
  Effect.map((attempt) => attempt !== null),
  Effect.scoped,
  Effect.catchCause((cause) => Cause.hasInterrupts(cause)
    ? Effect.failCause(cause)
    : Effect.logWarning(why, { environmentId, cause }).pipe(Effect.as(false))),
);


/**
 * Deletes a saved Branch unless it was kept meanwhile; false when it isn't deleted, and the Save stands. Under every queue
 * the teardown takes, each active attempt is cancelled first, so a Branch deletes even while it deploys.
 */
export const tryCloseBranch = (environmentId: string) => Effect.gen(function* () {
  const database = yield* Database;
  return yield* tryClose(environmentId, "A saved Branch was not deleted.",
    closeBranch(environmentId, "saved").pipe(Effect.flatMap((close) => database.transaction(Effect.gen(function* () {
      const row = yield* lockBranchScope(close.projectId, environmentId, "update");
      if (!row || row.kept) return null;
      yield* lockEnvironmentDeploymentQueues(close.environmentIds);
      yield* cancelActiveDeployments(close.environmentIds);
      return yield* close.admit;
    })))));
});

/** Tears down a PR Environment with its pull request, kept or not; null when its teardown already runs. */
export const closePrEnvironment = (environmentId: string) => Effect.gen(function* () {
  const database = yield* Database;
  const close = yield* closeBranch(environmentId, "pull_request_closed");
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockBranchScope(close.projectId, environmentId, "update");
    yield* lockEnvironmentDeploymentQueues(close.environmentIds);
    if ((yield* activeTeardownFor([environmentId])).size > 0) return null;
    return yield* close.admit;
  }));
}).pipe(Effect.scoped);

/**
 * A kept Branch stays after saving and never closes for being idle. Under its Branch row, which a close holds while it
 * admits, so Keep never reports success for a Branch already closing.
 */
export const setBranchKept = Effect.fn("Branches.setKept")(function* (actor: Actor, input: SetBranchKept) {
  const organization = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const where = and(
      eq(schemaEnvironmentBranch.environmentId, input.environmentId),
      eq(schemaEnvironmentBranch.organizationId, organization.id),
    );
    const [branch] = yield* drizzle.select({ projectId: schemaEnvironmentBranch.projectId }).from(schemaEnvironmentBranch).where(where);
    if (branch === undefined) return yield* new NotFound({ message: "The branch was not found." });
    if ((yield* lockBranchScope(branch.projectId, input.environmentId, "update")) === null) {
      return yield* new NotFound({ message: "The branch was not found." });
    }
    if ((yield* activeTeardownFor([input.environmentId])).size > 0) {
      return yield* new Conflict({ message: "This branch is already closing." });
    }
    yield* drizzle.update(schemaEnvironmentBranch).set({ kept: input.kept }).where(where);
    const [row] = yield* loadBranchRows(where);
    if (row === undefined) return yield* new NotFound({ message: "The branch was not found." });
    return row;
  }));
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
    closeBranch(environmentId, "idle").pipe(Effect.flatMap((close) => database.transaction(Effect.gen(function* () {
      // The rule again, in lock order (lockProjectDefault): the Project (a new Branch of it and a Default change wait),
      // the Branch row (Keep, Save and Update wait), then the queues the teardown takes (a deploy waits).
      yield* lockBranchScope(close.projectId, environmentId, "update");
      yield* lockEnvironmentDeploymentQueues(close.environmentIds);
      if (!(yield* idleBranches(now, environmentId)).includes(environmentId)) return null;
      return yield* close.admit;
    })))),
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
  const closing = yield* activeTeardownFor(branchIds);
  return dueForIdleClose({
    branches,
    latestAttemptAt: new Map(latest.flatMap((row) => (row.at === null ? [] : [[row.environmentId, row.at]]))),
    defaultEnvironmentIds: new Set(defaults.flatMap((row) => (row.environmentId === null ? [] : [row.environmentId]))),
  }, now).filter((environmentId) => !closing.has(environmentId));
});
