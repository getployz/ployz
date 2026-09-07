import "@tanstack/react-start/server-only";

import type { DeployIntent, PreparedDeploy } from "@ployz/sdk";
import { isDeepStrictEqual } from "node:util";
import { Data, Effect, Schema } from "effect";
import type { EnvironmentDeploymentPreview } from "#/modules/deployments/tables";
import { DeployImageNotPullableError } from "#/modules/deployments/deployment-errors";
import { findUnpullableSdkDeployImages } from "#/modules/deployments/image-gate";
import {
  loadDeploymentContext,
  loadResolvedDeployEnv,
  persistSdkDeployPreview,
  type DeploymentContext,
} from "#/modules/deployments/runtime-repository.server";
import { UnsupportedDeploymentSourceError } from "#/modules/deployments/runtime-contract";
import {
  compileSdkDeployIntent,
  projectRuntimeDeployPreview,
  runtimeDeployOutcomeSchema,
  runtimeDeployPreviewSchema,
} from "#/modules/deployments/runtime-preview";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { strictParseOptions } from "#/modules/environment-design/schema";

export type DeploymentRuntimeOutcome = typeof runtimeDeployOutcomeSchema.Type;

type SdkPreparedPreviewInput = {
  readonly project_name: PreparedDeploy["project_name"];
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
    | "sdk_outcome_invalid"
    | "sdk_preview_changed";
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
          cause instanceof UnsupportedDeploymentSourceError ||
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
          message: "The immutable deployment target could not be compiled.",
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
  const decoded = yield* Schema.decodeUnknownEffect(runtimeDeployPreviewSchema)(
    value,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      (cause) =>
        new DeploymentRuntimeInvalid({
          failureCode: "sdk_preview_invalid",
          message: "SDK deploy preview is invalid.",
          cause,
        }),
    ),
  );
  const preview = projectRuntimeDeployPreview(decoded);
  if (preview === null) {
    return yield* new DeploymentRuntimeInvalid({
      failureCode: "sdk_preview_invalid",
      message: "SDK deploy preview is invalid.",
    });
  }
  return preview;
});

export const previewRuntimeIntent = Effect.fn(
  "Deployments.previewRuntimeIntent",
)(function* (organizationId: string, intent: DeployIntent) {
    const sdk = yield* connectedRuntime(organizationId);
    const prepared = yield* sdk.preview(intent);
    const previewWithoutNewVolumes = {
      project_name: prepared.project_name,
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

export const previewEnvironmentDeployment = Effect.fn(
  "Deployments.previewEnvironmentDeployment",
)(function* (context: DeploymentContext) {
  const intent = yield* compileRuntimeIntent(context);
  const { preview } = yield* previewRuntimeIntent(
    context.organization.id,
    intent,
  );
  yield* persistSdkDeployPreview({
    environmentDeploymentId: context.deployment.id,
    preview,
  });
  return preview satisfies EnvironmentDeploymentPreview;
});

export const confirmRuntimeIntent = Effect.fn(
  "Deployments.confirmRuntimeIntent",
)(function* (
  organizationId: string,
  intent: DeployIntent,
  persisted: EnvironmentDeploymentPreview,
) {
  const { prepared, preview } = yield* previewRuntimeIntent(
    organizationId,
    intent,
  );
  if (!isDeepStrictEqual(preview, persisted)) {
    return yield* new DeploymentRuntimeInvalid({
      failureCode: "sdk_preview_changed",
      message: "The runtime deploy preview changed. Preview again.",
    });
  }
  const outcome = yield* prepared.confirm();
  return yield* Schema.decodeUnknownEffect(runtimeDeployOutcomeSchema)(outcome, {
    onExcessProperty: "ignore",
  }).pipe(
    Effect.mapError(
      (cause) =>
        new DeploymentRuntimeInvalid({
          failureCode: "sdk_outcome_invalid",
          message: "SDK deploy outcome is invalid.",
          cause,
        }),
    ),
  );
});

export const confirmEnvironmentDeployment = Effect.fn(
  "Deployments.confirmEnvironmentDeployment",
)(function* (context: DeploymentContext) {
  const persisted = yield* decodeSdkDeployPreview(
    context.deployment.deployPreview ?? null,
  );
  const intent = yield* compileRuntimeIntent(context);
  return yield* confirmRuntimeIntent(
    context.organization.id,
    intent,
    persisted,
  );
});

export const confirmLatestEnvironmentDeployment = Effect.fn(
  "Deployments.confirmLatestEnvironmentDeployment",
)(function* (environmentDeploymentId: string) {
  const context = yield* loadDeploymentContext(environmentDeploymentId);
  if (!context) {
    return yield* new DeploymentRuntimeInvalid({
      failureCode: "sdk_preview_invalid",
      message: "Environment deployment was not found.",
    });
  }
  return yield* confirmEnvironmentDeployment(context);
});
