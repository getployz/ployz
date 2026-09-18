import "@tanstack/react-start/server-only";

import { projectRuntimeOutcome } from "@ployz/sdk/config";
import type { DeployEvent, DeployIntent, PreparedDeploy } from "@ployz/sdk";
import { Cause, Data, Effect, Exit, Redacted, Schema } from "effect";
import { eq } from "drizzle-orm";
import { environmentDeployment } from "./tables";
import type { EnvironmentDeploymentPreview } from "#/modules/deployments/tables";
import { DeployImageNotPullableError } from "#/modules/deployments/deployment-errors";
import { findUnpullableSdkDeployImages } from "#/modules/deployments/image-gate";
import {
  loadDeploymentContext,
  loadResolvedDeployEnv,
  persistSdkDeployPreview,
  persistSdkDeployOutcome,
  markDeploymentStatus,
  type DeploymentContext,
} from "#/modules/deployments/runtime-repository.server";
import {
  compileSdkDeployIntent,
  parseSdkDeployPreview,
} from "#/modules/deployments/runtime-preview";
import { Database } from "#/server/database.server";
import { errorEvidenceFrom } from "#/lib/error-evidence";
import { persistDeploymentProgress } from "./deployment-events.server";
import { deploymentProgressForEvent, type DeploymentProgress } from "./deployment-progress";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";

export type DeploymentRuntimeOutcome = Effect.Success<ReturnType<typeof executeRuntimeIntent>>["outcome"];

type SdkPreparedPreviewInput = {
  readonly project_name: PreparedDeploy["project_name"];
  readonly storage?: PreparedDeploy["storage"];
  readonly prune_refusal?: PreparedDeploy["prune_refusal"];
  readonly operations: PreparedDeploy["operations"];
  readonly warnings: PreparedDeploy["warnings"];
  readonly would_remove: PreparedDeploy["would_remove"];
  readonly volumes_to_create?: PreparedDeploy["volumes_to_create"];
  readonly preserved_volumes: PreparedDeploy["preserved_volumes"];
};
type SdkDeployPreviewInput =
  | EnvironmentDeploymentPreview
  | SdkPreparedPreviewInput
  | null;

export class DeploymentRuntimeUnavailable extends Data.TaggedError(
  "DeploymentRuntimeUnavailable",
)<{
  readonly failureCode: "runtime_not_connected" | "runtime_unreachable";
  readonly message: string;
}> {
  get retriable() {
    return this.failureCode !== "runtime_not_connected";
  }
}

export class DeploymentRuntimeInvalid extends Data.TaggedError(
  "DeploymentRuntimeInvalid",
)<{
  readonly failureCode:
    | "deploy_image_not_pullable"
    | "sdk_preview_invalid"
    | "sdk_outcome_invalid";
  readonly message: string;
  readonly cause?: unknown;
}> {
  readonly retriable = false as const;
}

function compileRuntimeIntent(context: DeploymentContext) {
  return Effect.gen(function* () {
    const blocked = findUnpullableSdkDeployImages(
      context.snapshots.map((snapshot) => ({
        id: snapshot.serviceId,
        name: snapshot.config.name,
        source: snapshot.config.source,
      })),
    );
    if (blocked) {
      return yield* new DeploymentRuntimeInvalid({
        failureCode: "deploy_image_not_pullable",
        message: blocked.message,
        cause: blocked,
      });
    }
    const resolvedEnv = yield* loadResolvedDeployEnv(context);
    return yield* Effect.try({
      try: () =>
        compileSdkDeployIntent({
          projectName: context.environment.namespace,
          snapshots: context.snapshots.map((snapshot) => ({
            ...snapshot,
            resolvedEnv: resolvedEnv.get(snapshot.serviceId),
          })),
          volumes: context.volumes,
        }),
      catch: (cause) => {
        if (
          cause instanceof DeployImageNotPullableError
        ) {
          return new DeploymentRuntimeInvalid({
            failureCode: "deploy_image_not_pullable",
            message: cause.message,
            cause,
          });
        }
        return new DeploymentRuntimeInvalid({
          failureCode: "sdk_preview_invalid",
          message: cause instanceof Error ? cause.message : "The immutable deployment target could not be compiled.",
          cause,
        });
      },
    });
  });
}

function connectedRuntime(organizationId: string) {
  return Effect.gen(function* () {
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(organizationId);
    switch (session.status) {
      case "connected":
        return session.connected;
      case "no_connection":
        return yield* new DeploymentRuntimeUnavailable({
          failureCode: "runtime_not_connected",
          message: "The Organization has no connected runtime.",
        });
      case "unreachable":
        return yield* new DeploymentRuntimeUnavailable({
          failureCode: "runtime_unreachable",
          message: "The Organization runtime is unreachable.",
        });
      default: {
        const exhaustive: never = session;
        return exhaustive;
      }
    }
  });
}

export const decodeSdkDeployPreview = Effect.fn(
  "Deployments.decodeSdkDeployPreview",
)(function* (value: SdkDeployPreviewInput) {
  return yield* Effect.try({
    try: () => parseSdkDeployPreview(value),
    catch: (cause) => new DeploymentRuntimeInvalid({
      failureCode: "sdk_preview_invalid", message: "SDK deploy preview is invalid.", cause,
    }),
  });
});

export const previewRuntimeIntent = Effect.fn(
  "Deployments.previewRuntimeIntent",
)(function* (organizationId: string, intent: DeployIntent) {
    const sdk = yield* connectedRuntime(organizationId);
    const prepared = yield* sdk.preview(intent);
    let previewInput: SdkPreparedPreviewInput = {
      project_name: prepared.project_name,
      operations: prepared.operations,
      warnings: prepared.warnings,
      would_remove: prepared.would_remove,
      preserved_volumes: prepared.preserved_volumes,
    };
    if (prepared.storage !== undefined) {
      previewInput = { ...previewInput, storage: prepared.storage };
    }
    if (prepared.prune_refusal !== undefined) {
      previewInput = { ...previewInput, prune_refusal: prepared.prune_refusal };
    }
    if (prepared.volumes_to_create !== undefined) {
      previewInput = { ...previewInput, volumes_to_create: prepared.volumes_to_create };
    }
    const preview = yield* decodeSdkDeployPreview(previewInput);
    return { prepared, preview };
});

const confirmRuntimeIntent = Effect.fn("Deployments.confirmRuntimeIntent")(
  function* ({ prepared, preview }: Effect.Success<ReturnType<typeof previewRuntimeIntent>>, onEvent?: (event: DeployEvent) => Promise<void>, cancellation?: AbortSignal) {
  const outcome = yield* prepared.confirm(onEvent, cancellation);
  const evidence = yield* Schema.decodeUnknownEffect(Schema.Json)({ version: 1, outcome });
  const projected = yield* Effect.try({
    try: () => projectRuntimeOutcome(preview, evidence),
    catch: (cause) => new DeploymentRuntimeInvalid({
      failureCode: "sdk_outcome_invalid", message: "Runtime returned an invalid outcome; effects are unknown.", cause,
    }),
  });
  return { preview, outcome: projected.summary, evidence: Redacted.make(evidence) };
});

export const executeRuntimeIntent = Effect.fn("Deployments.executeRuntimeIntent")(
  function* (organizationId: string, intent: DeployIntent) {
    return yield* confirmRuntimeIntent(yield* previewRuntimeIntent(organizationId, intent));
  },
);

export const executeEnvironmentDeployment = Effect.fn(
  "Deployments.executeEnvironmentDeployment",
)(function* (context: DeploymentContext, expectedInngestRunId?: string) {
  if (context.deployment.inngestRunId !== (expectedInngestRunId ?? null)) {
    return yield* new DeploymentRuntimeInvalid({ failureCode: "sdk_outcome_invalid", message: "Deployment belongs to another workflow run." });
  }
  // Enter executing state inside the live SDK step so cancellation between
  // durable steps can still settle planning without waiting for a nonexistent worker.
  const started = yield* markDeploymentStatus({ environmentDeploymentId: context.deployment.id,
    expectedInngestRunId, status: "deploying" });
  if (!started) return yield* Effect.interrupt;
  return yield* Effect.gen(function* () {
    const intent = yield* compileRuntimeIntent(context);
    const prepared = yield* previewRuntimeIntent(context.organization.id, intent);
    yield* persistSdkDeployPreview({ environmentDeploymentId: context.deployment.id, expectedInngestRunId: context.deployment.inngestRunId ?? undefined, preview: prepared.preview });
    const database = yield* Database;
    const readStatus = database.drizzle.select({ status: environmentDeployment.status, cancellationRequestedAt: environmentDeployment.cancellationRequestedAt })
      .from(environmentDeployment).where(eq(environmentDeployment.id, context.deployment.id)).limit(1);
    const [current] = yield* readStatus;
    if (!current || current.status !== "deploying") return yield* Effect.interrupt;
    if (current.cancellationRequestedAt) {
      yield* markDeploymentStatus({ environmentDeploymentId: context.deployment.id,
        expectedInngestRunId, status: "cancelled", message: "Cancelled before runtime execution." });
      return { type: "failed" as const, completed: 0, unexecuted: prepared.prepared.operations.length, reason: "cancelled" as const };
    }
    const cancellation = new AbortController();
    // Inngest cancellation cannot interrupt an executing step. Check the durable
    // row even when the SDK emits no progress, then await its cleanup and outcome.
    const watchCancellation = Effect.gen(function* () {
      while (true) {
        const [deployment] = yield* readStatus;
        if (!deployment || deployment.cancellationRequestedAt) {
          cancellation.abort();
          return yield* Effect.never;
        }
        yield* Effect.sleep("1 second");
      }
    });
    let previous: DeploymentProgress | null = null;
    // The SDK reports progress through a Promise callback. Run each persist with
    // this fiber's services (database, tracer, span, log annotations) instead of
    // a fresh default runtime.
    const progressContext = yield* Effect.context<Database>();
    const persistProgress = Effect.runPromiseWith(progressContext);
    const { outcome, evidence } = yield* confirmRuntimeIntent(prepared, async (event) => {
      const raw = deploymentProgressForEvent(event, prepared.prepared.operations);
      const progress = { ...raw, rows: raw.rows.map((row) => {
        const prior = previous?.rows.find((candidate) => candidate.index === row.index);
        const projected = {
          ...row,
          serviceId: context.snapshots.find((snapshot) => snapshot.config.privateDns === row.serviceName)?.serviceId ?? null,
        };
        if (row.status === "failed" && prior) return { ...projected, phase: prior.phase, elapsedMs: prior.elapsedMs, deadlineMs: prior.deadlineMs, health: prior.health };
        return projected;
      }) };
      previous = progress;
      await persistProgress(persistDeploymentProgress(context.deployment.id, progress));
    }, cancellation.signal).pipe(Effect.raceFirst(watchCancellation));
    yield* persistSdkDeployOutcome({ environmentDeploymentId: context.deployment.id, expectedInngestRunId: context.deployment.inngestRunId ?? undefined, outcome: evidence });
    return outcome;
  }).pipe(Effect.onExit(exit => {
    if (Exit.isSuccess(exit)) return Effect.void;
    // A cancelled workflow cannot schedule another cleanup step. Settle here
    // even if preview, confirmation, or outcome decoding fails.
    const failure = errorEvidenceFrom(Cause.squash(exit.cause));
    return markDeploymentStatus({
      environmentDeploymentId: context.deployment.id, expectedInngestRunId,
      status: "failed", failureCode: failure.failureCode ?? "sdk_deploy_outcome_unknown",
      message: failure.message ?? "Runtime execution ended without a complete outcome; effects are unknown.",
    }).pipe(Effect.asVoid);
  }));
});

export const executeLatestEnvironmentDeployment = Effect.fn(
  "Deployments.executeLatestEnvironmentDeployment",
)(function* (environmentDeploymentId: string, expectedInngestRunId?: string) {
  const context = yield* loadDeploymentContext(environmentDeploymentId);
  if (!context) {
    return yield* new DeploymentRuntimeInvalid({
      failureCode: "sdk_preview_invalid",
      message: "Environment deployment was not found.",
    });
  }
  return yield* executeEnvironmentDeployment(context, expectedInngestRunId);
});
