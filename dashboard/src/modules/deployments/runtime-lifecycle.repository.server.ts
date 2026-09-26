import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNotNull, isNull, or, sql } from "drizzle-orm";
import { Effect, Option, Redacted, type Schema } from "effect";
import {
  environmentDeployment as schemaEnvironmentDeployment,
  environmentDeploymentImageBuild,
  environmentDeploymentSecret,
} from "#/modules/deployments/tables";
import { service as schemaService } from "#/modules/environment-design/tables";
import {
  environmentNodeConfigSnapshot,
  environmentNodeIntroduction as schemaEnvironmentNodeIntroduction,
} from "#/modules/runtime/tables";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { DeploymentExecutionError } from "#/modules/deployments/execution-error";
import { buildingAttemptOf, isActiveDeploymentUniqueViolation, lockEnvironmentDeploymentQueue, pendingAttemptOf } from "#/modules/deployments/queue-lock.server";
import {
  ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
  TERMINAL_ENVIRONMENT_DEPLOYMENT_STATUSES,
} from "#/modules/deployments/runtime-contract";
import {
  failAwaitingVolumeRemoveAttemptsForDeploymentInTransaction,
  releaseVolumeRemoveAttemptsForAppliedDeploymentInTransaction,
} from "#/modules/runtime/volume-removal.repository";
import { dispatchVolumeRemoveRequested } from "#/modules/runtime/volume-removal.server";
import { projectRuntimeOutcome } from "@ployz/sdk/config";
import { loadEnvironmentSnapshotProjection } from "./environment-state.repository.server";
import { writeTargetNodeList } from "./attempt-target.server";
import { coreOperationWatch } from "#/modules/operations/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import type { DeploymentProgress } from "./deployment-progress";
import type { SdkDeployPreview } from "./runtime-preview";
import { DeploymentQueueOccupied } from "./runtime-repository.contract";
import { Conflict } from "#/server/public-error";
import { type InngestClient, sendInngestEvent } from "#/modules/inngest/client";
import { createEnvironmentDeployRequestedEvent } from "#/modules/inngest/events";

function dispatchReleasedVolumeRemoveAttempts(
  attempts: readonly { id: string }[],
) {
  return afterDatabaseCommit(Effect.forEach(
    attempts,
    (attempt) =>
      dispatchVolumeRemoveRequested(attempt.id).pipe(
        Effect.catch((error) =>
          Effect.logError(
            "Failed to dispatch released volume removal",
            error,
          ).pipe(Effect.annotateLogs({ attemptId: attempt.id })),
        ),
      ),
    { concurrency: "unbounded", discard: true },
  ));
}

interface EnvironmentDeploymentStatusPatch {
  runtimeProgress?: DeploymentProgress;
  status: EnvironmentDeploymentStatus;
  updatedAt: Date;
  failureMessage?: string;
  failureCode?: string;
  startedAt?: Date;
  finishedAt?: Date;
  cancellationRequestedAt?: Date;
}

function lockDeploymentEnvironment(environmentDeploymentId: string) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [deployment] = yield* drizzle.select({ environmentId: schemaEnvironmentDeployment.environmentId })
      .from(schemaEnvironmentDeployment).where(eq(schemaEnvironmentDeployment.id, environmentDeploymentId));
    if (deployment) yield* lockEnvironmentDeploymentQueue(deployment.environmentId);
    return deployment?.environmentId;
  });
}

type DeploymentRunStatusChange = {
  runtimeProgress?: DeploymentProgress;
  environmentDeploymentId: string;
  status: Exclude<EnvironmentDeploymentStatus, "queued">;
  message?: string;
  failureCode?: string;
  /** Omit only for an unowned attempt. */
  expectedInngestRunId?: string;
};

type DeploymentTransition =
  | (DeploymentRunStatusChange & { kind: "run" })
  | { kind: "dispatch_failed"; environmentDeploymentId: string; status: "failed"; message: string; failureCode: string }
  | { kind: "cancel_before_execution"; environmentDeploymentId: string; status: "cancelled"; message: string; expectedInngestRunId?: string };

function transitionGuard(input: DeploymentTransition) {
  switch (input.kind) {
    case "dispatch_failed":
      return and(eq(schemaEnvironmentDeployment.status, "queued"), isNull(schemaEnvironmentDeployment.inngestRunId));
    case "cancel_before_execution":
      return and(inArray(schemaEnvironmentDeployment.status, ["queued", "planning"]),
        input.expectedInngestRunId ? eq(schemaEnvironmentDeployment.inngestRunId, input.expectedInngestRunId) : undefined);
    case "run":
      return and(
        input.expectedInngestRunId ? eq(schemaEnvironmentDeployment.inngestRunId, input.expectedInngestRunId) : isNull(schemaEnvironmentDeployment.inngestRunId),
        input.status === "planning"
          ? and(eq(schemaEnvironmentDeployment.status, "queued"), isNotNull(schemaEnvironmentDeployment.dispatchRequestedAt))
          : input.status === "deploying" ? eq(schemaEnvironmentDeployment.status, "planning")
          : inArray(schemaEnvironmentDeployment.status, [...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES]),
      );
  }
}

function markEnvironmentDeploymentStatus(input: DeploymentTransition) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const updatedAt = new Date();
    return yield* database.transaction(
      Effect.gen(function* () {
        const environmentId = yield* lockDeploymentEnvironment(input.environmentDeploymentId);
        const tx = (yield* Database).drizzle;
        const patch: EnvironmentDeploymentStatusPatch = {
          status: input.status,
          updatedAt,
        };
        if ("runtimeProgress" in input) patch.runtimeProgress = input.runtimeProgress;
        if (input.message) patch.failureMessage = input.message;
        if ("failureCode" in input && input.failureCode) patch.failureCode = input.failureCode;
        if (input.status === "cancelled") patch.cancellationRequestedAt = updatedAt;
        if (input.status === "planning") patch.startedAt = updatedAt;
        if (TERMINAL_ENVIRONMENT_DEPLOYMENT_STATUSES.has(input.status)) {
          patch.finishedAt = updatedAt;
        }
        const updated = yield* tx
          .update(schemaEnvironmentDeployment)
          .set(patch)
          .where(
            and(
              eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
              transitionGuard(input),
            ),
          )
          .returning({ id: schemaEnvironmentDeployment.id });
        if (updated.length === 0) return null;
        // An attempt leaving queued (building → planning, failed, cancelled) frees the queue for the pending attempt.
        if (environmentId) yield* dispatchPendingAfterCommit(environmentId);
        // An ended attempt stops its Image Builds; a running build step observes this and aborts.
        if (TERMINAL_ENVIRONMENT_DEPLOYMENT_STATUSES.has(input.status)) {
          yield* tx.update(environmentDeploymentImageBuild).set({ status: "cancelled", finishedAt: updatedAt, updatedAt })
            .where(and(eq(environmentDeploymentImageBuild.deploymentId, input.environmentDeploymentId), eq(environmentDeploymentImageBuild.status, "building")));
        }
        if (input.status === "cancelled") {
          yield* tx.update(coreOperationWatch).set({ observationState: "cloud_cancelled", terminalAt: updatedAt, updatedAt })
            .where(and(eq(coreOperationWatch.observationState, "active"), sql`exists (
              select 1 from ${schemaEnvironmentDeployment} deployment
              where deployment.id = ${input.environmentDeploymentId}
                and deployment.organization_id = ${coreOperationWatch.organizationId}
                and deployment.core_deploy_id = ${coreOperationWatch.operationId}
            )`));
        }
        if (input.status === "applied") {
          yield* settleConfirmedNodes(input.environmentDeploymentId);
          return yield* releaseVolumeRemoveAttemptsForAppliedDeploymentInTransaction(
            tx,
            input.environmentDeploymentId,
          );
        }
        if (input.status === "failed" || input.status === "cancelled") {
          yield* failAwaitingVolumeRemoveAttemptsForDeploymentInTransaction(
            tx,
            {
              environmentDeploymentId: input.environmentDeploymentId,
              deploymentDisposition: input.status,
              now: updatedAt,
            },
          );
        }
        return [];
      }),
    );
  });
}

export const recordInngestRun = Effect.fn("Deployments.recordInngestRun")(
  function* (input: { environmentDeploymentId: string; runId: string }) {
    const { drizzle } = yield* Database;
    const claimed = yield* drizzle
      .update(schemaEnvironmentDeployment)
      .set({ inngestRunId: input.runId, updatedAt: new Date() })
      .where(
        and(
          eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
          eq(schemaEnvironmentDeployment.status, "queued"),
          isNotNull(schemaEnvironmentDeployment.dispatchRequestedAt),
          or(
            isNull(schemaEnvironmentDeployment.inngestRunId),
            eq(schemaEnvironmentDeployment.inngestRunId, input.runId),
          ),
          isNull(schemaEnvironmentDeployment.cancellationRequestedAt),
        ),
      )
      .returning({ id: schemaEnvironmentDeployment.id });
    return claimed.length === 1;
  },
);

export const ownsDeploymentRun = Effect.fn("Deployments.ownsDeploymentRun")(
  function* (input: { environmentDeploymentId: string; inngestRunId: string }) {
    const { drizzle } = yield* Database;
    const [owned] = yield* drizzle
      .select({ id: schemaEnvironmentDeployment.id })
      .from(schemaEnvironmentDeployment)
      .where(
        and(
          eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
          eq(schemaEnvironmentDeployment.inngestRunId, input.inngestRunId),
          inArray(schemaEnvironmentDeployment.status, [
            ...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
          ]),
        ),
      )
      .limit(1);
    return Boolean(owned);
  },
);

export const markDeploymentFailedIfOwned = Effect.fn("Deployments.markDeploymentFailedIfOwned")(
  (input: { environmentDeploymentId: string; expectedInngestRunId: string; message: string; failureCode?: string }) =>
    markDeploymentStatus({ ...input, status: "failed" }),
);

export const failUndispatchedDeployment = Effect.fn("Deployments.failUndispatchedDeployment")(
  (input: { environmentDeploymentId: string; message: string; failureCode: string }) =>
    markEnvironmentDeploymentStatus({ ...input, kind: "dispatch_failed", status: "failed" }).pipe(Effect.map(changed => changed !== null)),
);

/** User cancellation may settle any owner only before execution starts. */
export const cancelDeploymentBeforeExecution = Effect.fn("Deployments.cancelDeploymentBeforeExecution")(
  (input: { environmentDeploymentId: string; expectedInngestRunId?: string; message: string }) =>
    markEnvironmentDeploymentStatus({ ...input, kind: "cancel_before_execution", status: "cancelled" }).pipe(Effect.map(changed => changed !== null)),
);

/** Derive node cleanup from the same Rust evidence used by Applied State. */
function settleConfirmedNodes(environmentDeploymentId: string, confirmedNodeIds?: readonly string[]) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [deployment] = yield* drizzle.select().from(schemaEnvironmentDeployment)
      .where(eq(schemaEnvironmentDeployment.id, environmentDeploymentId));
    if (!deployment) return;
    const nodes = yield* drizzle.select().from(environmentNodeConfigSnapshot).where(and(
      eq(environmentNodeConfigSnapshot.environmentDeploymentId, environmentDeploymentId),
      confirmedNodeIds ? inArray(environmentNodeConfigSnapshot.nodeId, [...confirmedNodeIds]) : undefined,
    ));
    yield* drizzle.delete(schemaEnvironmentNodeIntroduction).where(and(
      eq(schemaEnvironmentNodeIntroduction.environmentId, deployment.environmentId),
      sql`exists (
        select 1 from ${environmentNodeConfigSnapshot} snapshot
        where snapshot.environment_deployment_id = ${environmentDeploymentId}
          and snapshot.node_type = ${schemaEnvironmentNodeIntroduction.nodeType}
          and snapshot.node_id = ${schemaEnvironmentNodeIntroduction.nodeId}
          ${confirmedNodeIds ? sql`and ${inArray(sql`snapshot.node_id`, [...confirmedNodeIds])}` : sql``}
      )`,
    ));
    const serviceIds = nodes.filter(node => node.nodeType === "service").map(node => node.nodeId);
    if (serviceIds.length) yield* drizzle.update(schemaService).set({ firstDeployedAt: new Date() })
      .where(and(inArray(schemaService.id, serviceIds), isNull(schemaService.firstDeployedAt)));
  });
}

// Operation specs and errors can contain credentials; retain the complete SDK
// evidence only in the existing server-only encrypted attempt record.
export const persistSdkDeployOutcome = Effect.fn(
  "Deployments.persistSdkDeployOutcome",
)(function* (input: {
  environmentDeploymentId: string;
  outcome: Redacted.Redacted<Schema.Json>;
  runtimeProgress?: DeploymentProgress;
  expectedInngestRunId?: string;
}) {
  const database = yield* Database;
  const encryption = yield* SecretEncryption;
  const released = yield* database.transaction(Effect.gen(function* () {
    yield* lockDeploymentEnvironment(input.environmentDeploymentId);
    const { drizzle } = yield* Database;
    const [deployment] = yield* drizzle.select().from(schemaEnvironmentDeployment)
      .where(and(eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
        input.expectedInngestRunId ? eq(schemaEnvironmentDeployment.inngestRunId, input.expectedInngestRunId) : isNull(schemaEnvironmentDeployment.inngestRunId)))
      .for("update");
    if (!deployment) return [];
    const rows = yield* drizzle.update(environmentDeploymentSecret).set({
      encryptedRuntimeOutcome: encryption.encrypt(JSON.stringify(Redacted.value(input.outcome))),
    }).where(and(eq(environmentDeploymentSecret.environmentDeploymentId, input.environmentDeploymentId),
      isNull(environmentDeploymentSecret.encryptedRuntimeOutcome))).returning({ id: environmentDeploymentSecret.environmentDeploymentId });
    if (!rows.length) {
      const [retained] = yield* drizzle.select().from(environmentDeploymentSecret)
        .where(eq(environmentDeploymentSecret.environmentDeploymentId, input.environmentDeploymentId));
      if (!retained) return yield* new DeploymentExecutionError({ message: "Deployment attempt record was not found; runtime outcome could not be retained.", failureCode: "sdk_deploy_outcome_unknown" });
      return [];
    }
    const projected = yield* Effect.try({
      try: () => projectRuntimeOutcome(deployment.deployPreview, Redacted.value(input.outcome)),
      catch: () => new DeploymentExecutionError({ message: "Invalid runtime outcome; effects are unknown.", failureCode: "sdk_deploy_outcome_unknown" }),
    }).pipe(Effect.option);
    if (Option.isNone(projected)) {
      yield* markEnvironmentDeploymentStatus({
        kind: "run", environmentDeploymentId: input.environmentDeploymentId, expectedInngestRunId: input.expectedInngestRunId,
        status: "failed", message: "Invalid runtime outcome; effects are unknown.", failureCode: "sdk_deploy_outcome_unknown",
      });
      return [];
    }
    const outcome = projected.value.summary;
    const released = yield* markEnvironmentDeploymentStatus({
      kind: "run", environmentDeploymentId: input.environmentDeploymentId,
      expectedInngestRunId: input.expectedInngestRunId,
      runtimeProgress: input.runtimeProgress,
      status: outcome.type === "success" ? "applied" : outcome.reason === "cancelled" ? "cancelled" : "failed",
      message: outcome.type === "failed" ? `Deployment stopped (${outcome.reason}): ${outcome.completed} operations completed; ${outcome.unexecuted} not attempted. The failed operation may have additional effects.` : undefined,
      failureCode: outcome.type === "failed" ? "sdk_deploy_failed" : undefined,
    });
    if (outcome.type !== "success" || released === null) {
      const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: deployment.environmentId });
      const confirmed = [...projection.appliedSavedNodeByKey.values()].filter(node => node.sourceSavedStateSnapshotId === deployment.savedStateSnapshotId);
      yield* settleConfirmedNodes(input.environmentDeploymentId, confirmed.map(node => node.nodeId));
    }
    return released ?? [];
  }));
  yield* dispatchReleasedVolumeRemoveAttempts(released);
});

/** Record Image Cleanup on a terminal Deployment; its status never changes. */
export const persistImageCleanup = Effect.fn("Deployments.persistImageCleanup")(function* (
  environmentDeploymentId: string, imageCleanup: NonNullable<DeploymentProgress["imageCleanup"]>,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(schemaEnvironmentDeployment)
    .set({ runtimeProgress: sql`jsonb_set(${schemaEnvironmentDeployment.runtimeProgress}, '{imageCleanup}', ${JSON.stringify(imageCleanup)}::jsonb)` })
    .where(and(eq(schemaEnvironmentDeployment.id, environmentDeploymentId), isNotNull(schemaEnvironmentDeployment.runtimeProgress)));
});

export const persistSdkDeployPreview = Effect.fn(
  "Deployments.persistSdkDeployPreview",
)(function* (input: {
  environmentDeploymentId: string;
  preview: SdkDeployPreview;
  expectedInngestRunId?: string;
}) {
  const { drizzle } = yield* Database;
  const planned = yield* drizzle
    .update(schemaEnvironmentDeployment)
    .set({ deployPreview: input.preview, updatedAt: new Date() })
    .where(
      and(
        eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
        input.expectedInngestRunId ? eq(schemaEnvironmentDeployment.inngestRunId, input.expectedInngestRunId) : isNull(schemaEnvironmentDeployment.inngestRunId),
        inArray(schemaEnvironmentDeployment.status, [
          ...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
        ]),
      ),
    )
    .returning({ id: schemaEnvironmentDeployment.id });
  if (planned.length === 0) {
    return yield* Effect.fail(
      new DeploymentExecutionError({
        message: "Deployment planning lost a terminal-state race.",
        failureCode: "cloud_attempt_terminal",
      }),
    );
  }
});

export const markDeploymentStatus = Effect.fn(
  "Deployments.markDeploymentStatus",
)(function* (input: DeploymentRunStatusChange) {
  const changed = yield* markEnvironmentDeploymentStatus({ ...input, kind: "run" }).pipe(
    Effect.catchIf(
      isActiveDeploymentUniqueViolation,
      (cause) => new DeploymentQueueOccupied({ cause }),
    ),
  );
  if (!changed) return false;
  if (input.status === "applied") {
    yield* dispatchReleasedVolumeRemoveAttempts(changed);
  }
  return true;
});

export const beginEnvironmentDeploymentPlanning = Effect.fn(
  "Deployments.beginEnvironmentDeploymentPlanning",
)(function* (input: { readonly environmentDeploymentId: string; readonly expectedInngestRunId?: string }) {
  const database = yield* Database;
  const changed = yield* database.transaction(Effect.gen(function* () {
    const started = yield* markDeploymentStatus({ ...input, status: "planning" });
    if (!started) return false;
    // Starting freezes the target node list with the Attempt Target: rediffed against the whole of Applied State now.
    const environmentId = yield* lockDeploymentEnvironment(input.environmentDeploymentId);
    if (environmentId) {
      const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId });
      yield* writeTargetNodeList(input.environmentDeploymentId, projection.appliedSavedNodeByKey);
    }
    return true;
  })).pipe(
    Effect.catchTag("DeploymentQueueOccupied", () =>
      Effect.succeed("blocked" as const),
    ),
  );
  if (changed === "blocked") return { state: "blocked" as const };
  return { state: changed ? ("started" as const) : ("unavailable" as const) };
});

export const ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_CODE =
  "inngest_dispatch_failed";
export const ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_MESSAGE =
  "Cloud could not dispatch the deployment workflow.";

export type EnvironmentDeploymentDispatchInput = {
  readonly environmentDeploymentId: string;
  readonly environmentId: string;
};

const hasBuildingAttempt = Effect.fn("Deployments.hasBuildingAttempt")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [building] = yield* drizzle.select({ id: schemaEnvironmentDeployment.id }).from(schemaEnvironmentDeployment)
    .where(buildingAttemptOf(environmentId)).limit(1);
  return building !== undefined;
});

/**
 * Dispatches a committed queued row that is still unowned. Replays enqueue deliberately: the event has a
 * deterministic deployment ID, so Inngest owns deduplication across a crash after admission commit or
 * after event send. While a building attempt exists, the row is the pending attempt and stays undispatched.
 */
const sendEnvironmentDeployment = Effect.fn(
  "Deployments.sendEnvironmentDeployment",
)(function* (input: EnvironmentDeploymentDispatchInput) {
  const { drizzle } = yield* Database;
  // Read after the admission committed and outside the Environment lock, which is safe: the building
  // attempt leaves queued in its own committed transition, which then dispatches the pending attempt.
  // Either this read sees the building attempt gone and sends, or that transition's post-commit dispatch
  // sees our committed row. At worst both send, and the event ID deduplicates.
  if (yield* hasBuildingAttempt(input.environmentId)) return;
  const requestedAt = new Date();
  const requested = yield* drizzle
    .update(schemaEnvironmentDeployment)
    .set({ dispatchRequestedAt: requestedAt, updatedAt: requestedAt })
    .where(and(eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId), pendingAttemptOf(input.environmentId)))
    .returning({ id: schemaEnvironmentDeployment.id });
  if (requested.length === 0) {
    return yield* new Conflict({
      message: "The queued deployment is no longer available to dispatch.",
    });
  }

  yield* sendInngestEvent(createEnvironmentDeployRequestedEvent(input)).pipe(
    Effect.catchTag("InngestEventSendError", (failure) =>
      Effect.gen(function* () {
        const failed = yield* failUndispatchedDeployment({
          environmentDeploymentId: input.environmentDeploymentId,
          failureCode: ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_CODE,
          message: ENVIRONMENT_DEPLOYMENT_DISPATCH_FAILURE_MESSAGE,
        });
        if (failed) return yield* failure;
      }),
    ),
  );
});

/**
 * Dispatches after the outermost commit. `pending`: a building attempt holds the queue, so the attempt
 * waits until that one leaves queued; read now, and the send itself checks again after commit.
 */
export const dispatchEnvironmentDeployment = Effect.fn("Deployments.dispatchAfterCommit")(
  function* (input: EnvironmentDeploymentDispatchInput) {
    const pending = yield* hasBuildingAttempt(input.environmentId);
    yield* afterDatabaseCommit(sendEnvironmentDeployment(input));
    return { state: pending ? "pending" as const : "dispatched" as const };
  },
);

/**
 * An attempt leaving queued frees the queue for the pending attempt. Annotated: a failed dispatch
 * settles through markEnvironmentDeploymentStatus, which calls back here.
 */
function dispatchPendingAfterCommit(environmentId: string): Effect.Effect<void, never, Database | InngestClient> {
  return afterDatabaseCommit(dispatchPendingDeployment(environmentId).pipe(
    Effect.catch((error) => Effect.logError("Failed to dispatch the pending deployment", error)
      .pipe(Effect.annotateLogs({ environmentId }))),
  ));
}

/** Dispatches the Environment's pending attempt, unless a building attempt still holds the queue. */
export const dispatchPendingDeployment = Effect.fn("Deployments.dispatchPendingDeployment")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [pending] = yield* drizzle.select({ id: schemaEnvironmentDeployment.id }).from(schemaEnvironmentDeployment)
    .where(pendingAttemptOf(environmentId)).limit(1);
  if (!pending) return { state: "none" as const };
  return yield* dispatchEnvironmentDeployment({ environmentDeploymentId: pending.id, environmentId });
});

/**
 * Recovers a lost dispatch: sends every pending attempt whose Environment has no building attempt.
 * A pending attempt is normally dispatched when the building attempt leaves queued; a crash between
 * that commit and the send would otherwise strand it.
 */
export const dispatchStrandedPendingDeployments = Effect.fn("Deployments.dispatchStrandedPendingDeployments")(function* () {
  const { drizzle } = yield* Database;
  const stranded = yield* drizzle.select({ environmentId: schemaEnvironmentDeployment.environmentId })
    .from(schemaEnvironmentDeployment)
    .where(and(eq(schemaEnvironmentDeployment.status, "queued"), isNull(schemaEnvironmentDeployment.inngestRunId), sql`not exists (
      select 1 from ${schemaEnvironmentDeployment} building
      where building.environment_id = ${schemaEnvironmentDeployment.environmentId}
        and building.status = 'queued' and building.inngest_run_id is not null
    )`));
  yield* Effect.forEach(stranded, ({ environmentId }) => dispatchPendingDeployment(environmentId).pipe(
    Effect.catch((error) => Effect.logError("Failed to dispatch a stranded pending deployment", error)
      .pipe(Effect.annotateLogs({ environmentId })))), { discard: true });
  return stranded.length;
});
