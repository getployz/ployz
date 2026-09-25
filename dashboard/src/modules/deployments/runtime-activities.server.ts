import "@tanstack/react-start/server-only";

import { projectRuntimeOutcome } from "@ployz/sdk/config";
import type { DeployEvent, ImageRemovalOutcome, PreparedDeploy, PruneTarget } from "@ployz/sdk";
import { Cause, Data, Effect, Exit, Redacted, Schema } from "effect";
import { eq } from "drizzle-orm";
import { environmentDeployment } from "./tables";
import type { EnvironmentDeploymentPreview } from "#/modules/deployments/tables";
import {
  loadDeploymentContext,
  loadResolvedDeployEnv,
  persistSdkDeployPreview,
  persistImageCleanup,
  persistSdkDeployOutcome,
  markDeploymentStatus,
  type DeploymentContext,
} from "#/modules/deployments/runtime-repository.server";
import {
  compileSdkPreparationInput,
  parseSdkDeployPreview,
} from "#/modules/deployments/runtime-preview";
import { Database, ReportingDatabase } from "#/server/database.server";
import { errorEvidenceFrom } from "#/lib/error-evidence";
import { persistBuildLog, persistDeploymentProgress } from "./deployment-events.server";
import type { DeploymentProgress } from "./deployment-progress";
import { deploymentProgressForEvent } from "./deployment-view";
import { PloyzPreparationError, type PloyzPreparedDeploy } from "#/modules/runtime/ployz.server";
import { DeploymentExecutionError } from "./execution-error";
import { acquireDeploymentSources } from "./runtime-sources.server";
import { loadBuildReceipts, persistBuildReceipts } from "./build-receipts.server";
import { deploymentReporting } from "./deployment-reporting.server";
import { preparationProgressCollector } from "./preparation-progress";
import { lowerDeployment } from "@ployz/sdk/config";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { reserveClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { expandManagedHostnames } from "#/modules/environment-design/managed-hostnames.server";

export type DeploymentRuntimeOutcome = Effect.Success<ReturnType<typeof confirmRuntimeIntent>>["outcome"];

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
    | "sdk_preview_invalid"
    | "sdk_outcome_invalid";
  readonly message: string;
  readonly cause?: unknown;
}> {
  readonly retriable = false as const;
}

/** The Organization's Cluster Domain name. With no row, one inline reserve; a Hosted DNS failure refuses the deploy. */
const requireClusterDomain = (organizationId: string) =>
  reserveClusterDomain(organizationId).pipe(
    Effect.map((row) => row.name),
    Effect.catchTag("HostedDnsError", (cause) => Effect.fail(new DeploymentExecutionError({
      failureCode: "cluster_domain_unreserved",
      message: "The Organization has no Cluster Domain yet. Open Server Settings and choose Publish now, then deploy again.",
      cause,
    }))),
  );

function compileRuntimeIntent(context: DeploymentContext, clusterDomain: string | null) {
  return Effect.gen(function* () {
    const resolvedEnv = yield* loadResolvedDeployEnv(context, clusterDomain);
    // requireClusterDomain already refused a deploy with managed hostnames and no Cluster Domain.
    const snapshots = context.snapshots.map((snapshot) => ({
      ...snapshot,
      config: clusterDomain === null ? snapshot.config : expandManagedHostnames(snapshot.config, clusterDomain),
      resolvedEnv: resolvedEnv.get(snapshot.serviceId),
    }));
    return yield* Effect.try({
      try: () =>
        compileSdkPreparationInput({
          projectName: context.environment.namespace,
          snapshots,
          volumes: context.volumes,
          variableProducers: context.deployment.variableProducers ?? [],
        }),
      catch: (cause) => {
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

function preparedPreviewInput(prepared: SdkPreparedPreviewInput): SdkPreparedPreviewInput {
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
    return previewInput;
}

const confirmRuntimeIntent = Effect.fn("Deployments.confirmRuntimeIntent")(
  function* (prepared: PloyzPreparedDeploy, preview: Effect.Success<ReturnType<typeof decodeSdkDeployPreview>>, onEvent: (event: DeployEvent) => Promise<void>, cancellation: AbortSignal, deploymentId: string) {
  const outcome = yield* prepared.confirm(onEvent, cancellation, deploymentId);
  const evidence = yield* Schema.decodeUnknownEffect(Schema.Json)({ version: 1, outcome });
  const projected = yield* Effect.try({
    try: () => projectRuntimeOutcome(preview, evidence),
    catch: (cause) => new DeploymentRuntimeInvalid({
      failureCode: "sdk_outcome_invalid", message: "Runtime returned an invalid outcome; effects are unknown.", cause,
    }),
  });
  return { outcome: projected.summary, evidence: Redacted.make(evidence) };
});

/** Poll failure is fatal: a quiet operation must never outlive its cancellation observer. */
export function watchDeploymentCancellation<E, R>(
  readStatus: Effect.Effect<readonly { status: string; cancellationRequestedAt: Date | null }[], E, R>,
  cancellation: AbortController,
) {
  return Effect.gen(function* () {
    while (true) {
      const [deployment] = yield* readStatus;
      if (!deployment || deployment.status !== "deploying" || deployment.cancellationRequestedAt) {
        cancellation.abort();
        return yield* Effect.never;
      }
      yield* Effect.sleep("1 second");
    }
  }).pipe(
    Effect.tapError(() => Effect.sync(() => cancellation.abort())),
    Effect.mapError((cause) => new DeploymentExecutionError({
      failureCode: "sdk_deploy_outcome_unknown", message: "Cancellation monitoring failed; remote execution outcome is unknown.", cause,
    })),
  );
}

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
  const cancellation = new AbortController();
  let remoteStarted = false;
  const reporting = deploymentReporting();
  let latestProgress: DeploymentProgress = { completed: 0, total: 0, outcome: null, rows: [], compensation: [] };
  let pruneTargets: readonly PruneTarget[] = [];
  const terminalProgress = (): DeploymentProgress => {
    const progress: DeploymentProgress = { ...latestProgress, logsIncomplete: reporting.incomplete };
    if (!pruneTargets.length) return progress;
    return { ...progress, imageCleanup: { state: "running", machines: new Set(pruneTargets.map((t) => t.machine_id)).size, targets: pruneTargets } };
  };
  const reportProgress = (progress: DeploymentProgress) => {
    latestProgress = { ...progress, logsIncomplete: reporting.incomplete };
    return persistDeploymentProgress(context.deployment.id, latestProgress);
  };
  return yield* Effect.gen(function* () {
    const database = yield* Database;
    const readStatus = database.drizzle.select({ status: environmentDeployment.status, cancellationRequestedAt: environmentDeployment.cancellationRequestedAt })
      .from(environmentDeployment).where(eq(environmentDeployment.id, context.deployment.id)).limit(1);
    const [initial] = yield* readStatus;
    if (!initial || initial.status !== "deploying") return yield* Effect.interrupt;
    if (initial.cancellationRequestedAt) {
      return { outcome: { type: "failed" as const, completed: 0, unexecuted: 0, reason: "cancelled" as const }, evidence: null };
    }
    const watchCancellation = watchDeploymentCancellation(readStatus, cancellation);
    return yield* Effect.gen(function* () {
    const progressContext = yield* Effect.context<Database | ReportingDatabase>();
    const runReport = Effect.runPromiseWith(progressContext);
    const persistProgress = <A, E>(program: Effect.Effect<A, E, Database>) => runReport(reporting.write(program));
    const collector = preparationProgressCollector();
    const cancelled = Effect.callback<never>((resume) => {
      const abort = () => resume(Effect.interrupt);
      if (cancellation.signal.aborted) abort();
      else cancellation.signal.addEventListener("abort", abort, { once: true });
      return Effect.sync(() => cancellation.signal.removeEventListener("abort", abort));
    });
    const { sources, source_commits } = yield* acquireDeploymentSources(context, (serviceId) => reporting.write(reportProgress({
      completed: 0, total: 0, outcome: null, rows: [], compensation: [],
      preparation: { ...collector.current(), phase: "source", serviceId, message: "Acquiring source" },
    }))).pipe(Effect.raceFirst(cancelled));
    const sdk = yield* connectedRuntime(context.organization.id);
    const needsClusterDomain = context.snapshots.some(({ config }) => config.managedHostnames.length > 0);
    const clusterDomain = needsClusterDomain ? yield* requireClusterDomain(context.organization.id) : null;
    const input = yield* compileRuntimeIntent(context, clusterDomain);
    if (cancellation.signal.aborted) return yield* Effect.interrupt;
    remoteStarted = true;
    const build_receipts = Object.keys(sources).length === 0 ? {} : yield* loadBuildReceipts(context);
    const native = Object.keys(sources).length === 0
      ? yield* Effect.try({
          try: () => lowerDeployment(input),
          catch: (cause) => new DeploymentRuntimeInvalid({ failureCode: "sdk_preview_invalid", message: "The deployment settings are invalid.", cause }),
        }).pipe(Effect.flatMap((intent) => sdk.preview(intent)))
      : yield* sdk.prepare({ deployment: input, sources, source_commits, build_receipts }, async (event) => {
          const writes = collector.event(event);
          const progress = writes.progress
            ? reportProgress({ completed: 0, total: 0, outcome: null, rows: [], compensation: [], preparation: writes.progress })
            : Effect.void;
          await persistProgress(persistBuildLog(context.deployment.id, writes).pipe(Effect.andThen(progress)));
        }, cancellation.signal).pipe(
          Effect.tap(() => reporting.write(persistBuildLog(context.deployment.id, { steps: collector.finish(), output: [] }))),
          Effect.tapError((error) => {
            const failure = error instanceof PloyzPreparationError ? error : null;
            return reporting.write(persistBuildLog(context.deployment.id, { steps: collector.finish(failure?.message ?? "Preparation failed", failure?.stage ?? null), output: [] }).pipe(
              Effect.andThen(failure
                ? reportProgress({ completed: 0, total: 0, rows: [], outcome: null, compensation: [],
                    preparation: { ...collector.current(), message: failure.message, failureCode: failure.failureCode, stage: failure.stage, work: failure.work } })
                : Effect.void),
            ));
          }));
    if (Object.keys(sources).length > 0) yield* persistBuildReceipts(context, native.buildReceipts);
    const preview = yield* decodeSdkDeployPreview(preparedPreviewInput(native));
    yield* persistSdkDeployPreview({ environmentDeploymentId: context.deployment.id, expectedInngestRunId, preview });
    const [beforeConfirm] = yield* readStatus;
    if (!beforeConfirm || beforeConfirm.status !== "deploying" || beforeConfirm.cancellationRequestedAt || cancellation.signal.aborted) {
      return { outcome: { type: "failed" as const, completed: 0, unexecuted: native.operations.length, reason: "cancelled" as const }, evidence: null };
    }
    // The SDK reports progress through a Promise callback. Run each persist with
    // this fiber's services (database, tracer, span, log annotations) instead of
    // a fresh default runtime.
    pruneTargets = native.pruneTargets;
    const { outcome, evidence } = yield* confirmRuntimeIntent(native, preview, async (event) => {
      if (event.type === "images_pruned") return;
      const raw = deploymentProgressForEvent(event, native.operations, {
        prior: latestProgress,
        serviceIdFor: (serviceName) => context.snapshots.find((snapshot) => snapshot.config.privateDns === serviceName)?.serviceId ?? null,
      });
      const progress: DeploymentProgress = { ...raw, preparation: Object.keys(sources).length ? { ...collector.current(), phase: "ready" } : undefined };
      await persistProgress(reportProgress(progress));
    }, cancellation.signal, context.deployment.id);
    return { outcome, evidence };
    }).pipe(Effect.raceFirst(watchCancellation));
  }).pipe(Effect.scoped, Effect.flatMap(({ outcome, evidence }) => Effect.gen(function* () {
    // Only release the Environment slot after native and source finalizers settle.
    if (evidence) {
      yield* persistSdkDeployOutcome({ environmentDeploymentId: context.deployment.id, expectedInngestRunId, outcome: evidence, runtimeProgress: terminalProgress() });
    } else {
      yield* markDeploymentStatus({ environmentDeploymentId: context.deployment.id, expectedInngestRunId, status: "cancelled", runtimeProgress: terminalProgress(), message: "Cancelled before application execution." });
    }
    return outcome;
  })), Effect.onExit(exit => {
    if (Exit.isSuccess(exit)) return Effect.void;
    // A cancelled workflow cannot schedule another cleanup step. Settle here
    // even if preview, confirmation, or outcome decoding fails.
    const failure = errorEvidenceFrom(Cause.squash(exit.cause));
    const confirmedCancelled = failure.failureCode === "sdk_preparation_cancelled" || (!remoteStarted && cancellation.signal.aborted);
    return markDeploymentStatus({
      environmentDeploymentId: context.deployment.id, expectedInngestRunId,
      runtimeProgress: terminalProgress(),
      status: confirmedCancelled ? "cancelled" : "failed", failureCode: failure.failureCode ?? (remoteStarted ? "sdk_deploy_outcome_unknown" : "source_acquisition_failed"),
      message: confirmedCancelled ? "Cancelled before application execution." : failure.message || (remoteStarted ? "Runtime execution ended without a complete outcome; effects are unknown." : "Could not acquire deployment source."),
    }).pipe(Effect.asVoid);
  }));
});

const cleanOutcomes: ReadonlySet<ImageRemovalOutcome["status"]> = new Set(["removed", "in_use", "not_found"]);

/**
 * Image Cleanup after the Environment slot is released. Never fails and never
 * changes the Deployment status; a problem is a muted warning.
 */
export const cleanUpDeploymentImages = Effect.fn("Deployments.cleanUpDeploymentImages")(
  function* (environmentDeploymentId: string) {
    const context = yield* loadDeploymentContext(environmentDeploymentId);
    const cleanup = context?.deployment.runtimeProgress?.imageCleanup;
    if (!context || cleanup?.state !== "running") return;
    // SAFETY: these targets were written from the SDK's own PruneTarget list; MachineId is only a brand.
    const targets = cleanup.targets as readonly PruneTarget[];
    const clean = yield* connectedRuntime(context.organization.id).pipe(
      Effect.flatMap((sdk) => sdk.pruneImages(targets)),
      // Unsupported and unanswered Servers were not cleaned; say so rather than hide it.
      Effect.map((report) => report.machines.every(({ result }) =>
        result.status === "cleaned" && result.removals.every(({ outcome }) => cleanOutcomes.has(outcome.status)))),
      Effect.orElseSucceed(() => false),
    );
    yield* persistImageCleanup(environmentDeploymentId, { state: clean ? "cleaned" : "warning", machines: cleanup.machines });
  },
);

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
