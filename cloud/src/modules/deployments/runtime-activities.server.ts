import "@tanstack/react-start/server-only";

import { projectRuntimeOutcome } from "@ployz/sdk/config";
import type { DeployIntent, PreparedDeploy } from "@ployz/sdk";
import { Data, Effect, Redacted, Schema } from "effect";
import type { EnvironmentDeploymentPreview } from "#/modules/deployments/tables";
import { DeployImageNotPullableError } from "#/modules/deployments/deployment-errors";
import { findUnpullableSdkDeployImages } from "#/modules/deployments/image-gate";
import {
  loadDeploymentContext,
  loadResolvedDeployEnv,
  persistSdkDeployPreview,
  persistSdkDeployOutcome,
  type DeploymentContext,
} from "#/modules/deployments/runtime-repository.server";
import {
  compileSdkDeployIntent,
  parseSdkDeployPreview,
} from "#/modules/deployments/runtime-preview";
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
    const previewWithoutNewVolumes = {
      project_name: prepared.project_name,
      ...(prepared.storage === undefined ? {} : { storage: prepared.storage }),
      ...(prepared.prune_refusal === undefined ? {} : { prune_refusal: prepared.prune_refusal }),
      operations: prepared.operations,
      warnings: prepared.warnings,
      would_remove: prepared.would_remove,
      preserved_volumes: prepared.preserved_volumes,
    };
    const previewInput =
      prepared.volumes_to_create === undefined
        ? previewWithoutNewVolumes
        : {
            ...previewWithoutNewVolumes,
            volumes_to_create: prepared.volumes_to_create,
          };
    const preview = yield* decodeSdkDeployPreview(previewInput);
    return { prepared, preview };
});

const confirmRuntimeIntent = Effect.fn("Deployments.confirmRuntimeIntent")(
  function* ({ prepared, preview }: Effect.Success<ReturnType<typeof previewRuntimeIntent>>) {
  const outcome = yield* prepared.confirm();
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
)(function* (context: DeploymentContext) {
  const intent = yield* compileRuntimeIntent(context);
  const prepared = yield* previewRuntimeIntent(context.organization.id, intent);
  yield* persistSdkDeployPreview({ environmentDeploymentId: context.deployment.id, preview: prepared.preview });
  const { outcome, evidence } = yield* confirmRuntimeIntent(prepared);
  yield* persistSdkDeployOutcome({ environmentDeploymentId: context.deployment.id, outcome: evidence });
  return outcome;
});

export const executeLatestEnvironmentDeployment = Effect.fn(
  "Deployments.executeLatestEnvironmentDeployment",
)(function* (environmentDeploymentId: string) {
  const context = yield* loadDeploymentContext(environmentDeploymentId);
  if (!context) {
    return yield* new DeploymentRuntimeInvalid({
      failureCode: "sdk_preview_invalid",
      message: "Environment deployment was not found.",
    });
  }
  return yield* executeEnvironmentDeployment(context);
});
