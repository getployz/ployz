import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNotNull, isNull, or, sql } from "drizzle-orm";
import { Effect, Redacted, type Schema } from "effect";
import { environmentDeploymentSecret } from "#/modules/deployments/tables";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  service as schemaService,
} from "#/modules/environment-design/tables";
import {
  environmentNodeConfigSnapshot as schemaEnvironmentNodeConfigSnapshot,
  environmentNodeIntroduction as schemaEnvironmentNodeIntroduction,
} from "#/modules/runtime/tables";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { DeploymentExecutionError } from "#/modules/deployments/execution-error";
import { isActiveDeploymentUniqueViolation } from "#/modules/deployments/queue-lock.server";
import {
  ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
  TERMINAL_ENVIRONMENT_DEPLOYMENT_STATUSES,
  type EnvironmentDeploymentApplyResult,
} from "#/modules/deployments/runtime-contract";
import {
  failUnsubmittedDestructiveVolumeAttemptsForDeploymentInTransaction,
  releaseDestructiveVolumeAttemptsForAppliedDeploymentInTransaction,
} from "#/modules/operations/destructive-volume-attempt.repository";
import {
  dispatchDestructiveVolumeAttempt,
} from "#/modules/operations/destructive-volume-dispatch.server";
import { Database } from "#/server/database.server";
import type { SdkDeployPreview } from "./runtime-preview";
import { DeploymentQueueOccupied } from "./runtime-repository.contract";

function dispatchReleasedDestructiveVolumeAttempts(
  attempts: readonly { id: string }[],
) {
  return Effect.forEach(
    attempts,
    (attempt) =>
      dispatchDestructiveVolumeAttempt(attempt.id).pipe(
        Effect.catch((error) =>
          Effect.logError(
            "Failed to dispatch released destructive volume attempt",
            error,
          ).pipe(Effect.annotateLogs({ attemptId: attempt.id })),
        ),
      ),
    { concurrency: "unbounded", discard: true },
  );
}

function afterAppliedDeployment(
  environmentDeploymentId: string,
  released: readonly { id: string }[],
) {
  return Effect.gen(function* () {
    yield* dispatchReleasedDestructiveVolumeAttempts(released);
    yield* latchFirstDeployedAtForDeployment(environmentDeploymentId);
  });
}

function latchFirstDeployedAtForDeployment(environmentDeploymentId: string) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const snapshots = yield* drizzle
      .select({ serviceId: schemaEnvironmentNodeConfigSnapshot.nodeId })
      .from(schemaEnvironmentNodeConfigSnapshot)
      .where(
        and(
          eq(
            schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
            environmentDeploymentId,
          ),
          eq(schemaEnvironmentNodeConfigSnapshot.nodeType, "service"),
        ),
      );
    const serviceIds = snapshots.map((row) => row.serviceId);
    if (serviceIds.length === 0) return;
    yield* drizzle
      .update(schemaService)
      .set({ firstDeployedAt: new Date() })
      .where(
        and(
          inArray(schemaService.id, serviceIds),
          isNull(schemaService.firstDeployedAt),
        ),
      );
  });
}

interface EnvironmentDeploymentStatusPatch {
  status: EnvironmentDeploymentStatus;
  updatedAt: Date;
  failureMessage?: string;
  failureCode?: string;
  startedAt?: Date;
  finishedAt?: Date;
}

function markEnvironmentDeploymentStatus(input: {
  environmentDeploymentId: string;
  status: EnvironmentDeploymentStatus;
  message?: string;
  failureCode?: string;
}) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const updatedAt = new Date();
    return yield* database.transaction(
      Effect.gen(function* () {
        const tx = (yield* Database).drizzle;
        const patch: EnvironmentDeploymentStatusPatch = {
          status: input.status,
          updatedAt,
        };
        if (input.message) patch.failureMessage = input.message;
        if (input.failureCode) patch.failureCode = input.failureCode;
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
              input.status === "planning"
                ? and(
                    eq(schemaEnvironmentDeployment.status, "queued"),
                    isNotNull(schemaEnvironmentDeployment.dispatchRequestedAt),
                  )
                : inArray(schemaEnvironmentDeployment.status, [
                    ...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
                  ]),
            ),
          )
          .returning({ id: schemaEnvironmentDeployment.id });
        if (updated.length === 0) return null;
        if (input.status === "applied") {
          return yield* releaseDestructiveVolumeAttemptsForAppliedDeploymentInTransaction(
            tx,
            input.environmentDeploymentId,
          );
        }
        if (input.status === "failed" || input.status === "cancelled") {
          yield* failUnsubmittedDestructiveVolumeAttemptsForDeploymentInTransaction(
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

export const markDeploymentFailedIfOwned = Effect.fn(
  "Deployments.markDeploymentFailedIfOwned",
)(function* (input: {
  environmentDeploymentId: string;
  expectedInngestRunId: string;
  message: string;
  failureCode?: string;
}) {
  const database = yield* Database;
  const now = new Date();
  return yield* database.transaction(
    Effect.gen(function* () {
      const tx = (yield* Database).drizzle;
      const [deployment] = yield* tx
        .update(schemaEnvironmentDeployment)
        .set({
          status: "failed",
          failureMessage: input.message,
          failureCode: input.failureCode,
          finishedAt: now,
          updatedAt: now,
        })
        .where(
          and(
            eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
            eq(
              schemaEnvironmentDeployment.inngestRunId,
              input.expectedInngestRunId,
            ),
            inArray(schemaEnvironmentDeployment.status, [
              ...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
            ]),
          ),
        )
        .returning({ id: schemaEnvironmentDeployment.id });
      if (!deployment) return false;
      yield* failUnsubmittedDestructiveVolumeAttemptsForDeploymentInTransaction(
        tx,
        {
          environmentDeploymentId: input.environmentDeploymentId,
          deploymentDisposition: "failed",
          now,
        },
      );
      return true;
    }),
  );
});

// Operation specs and errors can contain credentials; retain the complete SDK
// evidence only in the existing server-only encrypted attempt record.
export const persistSdkDeployOutcome = Effect.fn(
  "Deployments.persistSdkDeployOutcome",
)(function* (input: {
  environmentDeploymentId: string;
  outcome: Redacted.Redacted<Schema.Json>;
}) {
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const rows = yield* drizzle.update(environmentDeploymentSecret)
    .set({ encryptedRuntimeOutcome: encryption.encrypt(JSON.stringify(Redacted.value(input.outcome))) })
    .where(eq(environmentDeploymentSecret.environmentDeploymentId, input.environmentDeploymentId))
    .returning({ id: environmentDeploymentSecret.environmentDeploymentId });
  if (rows.length === 0) {
    return yield* new DeploymentExecutionError({
      message: "Deployment attempt record was not found; runtime outcome could not be retained.",
      failureCode: "sdk_deploy_outcome_unknown",
    });
  }
});

export const persistSdkDeployPreview = Effect.fn(
  "Deployments.persistSdkDeployPreview",
)(function* (input: {
  environmentDeploymentId: string;
  preview: SdkDeployPreview;
}) {
  const { drizzle } = yield* Database;
  const planned = yield* drizzle
    .update(schemaEnvironmentDeployment)
    .set({ deployPreview: input.preview, updatedAt: new Date() })
    .where(
      and(
        eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
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

export const persistDeployApplyResult = Effect.fn(
  "Deployments.persistDeployApplyResult",
)(function* (input: {
  environmentDeploymentId: string;
  result: EnvironmentDeploymentApplyResult;
}) {
  const database = yield* Database;
  const applied = yield* database.transaction(
    Effect.gen(function* () {
      const tx = (yield* Database).drizzle;
      const updated = yield* tx
        .update(schemaEnvironmentDeployment)
        .set({
          status: "applied",
          coreDeployId: input.result.coreDeployId,
          failureCode: null,
          failureMessage: null,
          finishedAt: new Date(),
          updatedAt: new Date(),
        })
        .where(
          and(
            eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
            inArray(schemaEnvironmentDeployment.status, [
              ...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
            ]),
          ),
        )
        .returning({ id: schemaEnvironmentDeployment.id });
      if (updated.length === 0) return null;
      yield* tx
        .delete(schemaEnvironmentNodeIntroduction)
        .where(
          sql`exists (
                select 1
                from ${schemaEnvironmentNodeConfigSnapshot} snapshot
                where snapshot.environment_deployment_id = ${input.environmentDeploymentId}
                  and snapshot.environment_id = ${schemaEnvironmentNodeIntroduction.environmentId}
                  and snapshot.node_type = ${schemaEnvironmentNodeIntroduction.nodeType}
                  and snapshot.node_id = ${schemaEnvironmentNodeIntroduction.nodeId}
              )`,
        );
      return yield* releaseDestructiveVolumeAttemptsForAppliedDeploymentInTransaction(
        tx,
        input.environmentDeploymentId,
      );
    }),
  );
  if (!applied) return false;
  yield* afterAppliedDeployment(input.environmentDeploymentId, applied);
  return true;
});

export const markDeploymentStatus = Effect.fn(
  "Deployments.markDeploymentStatus",
)(function* (input: {
  environmentDeploymentId: string;
  status: EnvironmentDeploymentStatus;
  message?: string;
  failureCode?: string;
}) {
  const changed = yield* markEnvironmentDeploymentStatus(input).pipe(
    Effect.catchIf(
      isActiveDeploymentUniqueViolation,
      (cause) => new DeploymentQueueOccupied({ cause }),
    ),
  );
  if (!changed) return false;
  if (input.status === "applied") {
    yield* afterAppliedDeployment(input.environmentDeploymentId, changed);
  }
  return true;
});

export const beginEnvironmentDeploymentPlanning = Effect.fn(
  "Deployments.beginEnvironmentDeploymentPlanning",
)(function* (input: { readonly environmentDeploymentId: string }) {
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
