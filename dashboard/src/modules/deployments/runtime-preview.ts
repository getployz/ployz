import { deploymentDependencies } from "./deployment-dependencies";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
import type { DeployIntent } from "@ployz/sdk";
import { lowerDeployment, parseRuntimePreview } from "@ployz/sdk/config";
import { Schema } from "effect";
import type { EnvironmentDeploymentPreview } from "#/modules/deployments/tables";
import {
  type EnvironmentDeploySnapshot,
  type EnvironmentDeployVolume,
} from "#/modules/deployments/runtime-contract";

export const runtimeDeployPreviewSchema = Schema.Struct({
  storage: Schema.optional(Schema.mutable(Schema.Array(Schema.Json))),
  project_name: Schema.String.check(Schema.isNonEmpty()),
  prune_refusal: Schema.optionalKey(Schema.NullOr(Schema.Literals([
    "incomplete_snapshot", "selected_services",
  ]))),
  operations: Schema.mutable(Schema.Array(Schema.Json)),
  warnings: Schema.mutable(Schema.Array(Schema.Json)),
  would_remove: Schema.mutable(Schema.Array(Schema.Json)),
  volumes_to_create: Schema.optional(
    Schema.mutable(Schema.Array(Schema.Json)),
  ),
  preserved_volumes: Schema.mutable(Schema.Array(Schema.Json)),
});

export type SdkDeployPreview = EnvironmentDeploymentPreview;

export function compileSdkPreparationInput(input: {
  projectName: string;
  snapshots: readonly EnvironmentDeploySnapshot[];
  volumes?: readonly EnvironmentDeployVolume[];
  variableProducers?: readonly EnvironmentSnapshotVariableProducer[];
}) {
  const { variableProducers = [], ...deployment } = input;
  return { ...deployment, dependencies: deploymentDependencies(input.snapshots, variableProducers) };
}

export function parseSdkDeployPreview<T>(value: T): SdkDeployPreview {
  return parseRuntimePreview(value);
}

export function compileSdkDeployIntent(input: Parameters<typeof compileSdkPreparationInput>[0]): DeployIntent {
  return lowerDeployment(compileSdkPreparationInput(input));
}
