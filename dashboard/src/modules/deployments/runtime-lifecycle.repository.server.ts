import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNotNull, isNull, or, sql } from "drizzle-orm";
import { Effect, Option, Redacted, type Schema } from "effect";
import {
  environmentDeployment as schemaEnvironmentDeployment,
  environmentDeploymentSecret,
} from "#/modules/deployments/tables";
import { service as schemaService } from "#/modules/environment-design/tables";
import {
  environmentNodeConfigSnapshot,
  environmentNodeIntroduction as schemaEnvironmentNodeIntroduction,
} from "#/modules/runtime/tables";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { DeploymentExecutionError } from "#/modules/deployments/execution-error";
import { isActiveDeploymentUniqueViolation, lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
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
import { coreOperationWatch } from "#/modules/operations/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import type { DeploymentProgress } from "./deployment-progress";
import type { SdkDeployPreview } from "./runtime-preview";
import { DeploymentQueueOccupied } from "./runtime-repository.contract";

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
        yield* lockDeploymentEnvironment(input.environmentDeploymentId);
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
  const changed = yield* markDeploymentStatus({
    ...input,
    status: "planning",
  }).pipe(
    Effect.catchTag("DeploymentQueueOccupied", () =>
      Effect.succeed("blocked" as const),
    ),
  );
  if (changed === "blocked") return { state: "blocked" as const };
  return { state: changed ? ("started" as const) : ("unavailable" as const) };
});
