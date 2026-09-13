import { Effect, Schema } from "effect";
import {
  isEnvironmentResourceType,
  type EnvironmentResourceType,
} from "#/modules/environment-design/environment-resource-types";
import { decodeStrict, strictParseOptions } from "#/modules/environment-design/schema";
import {
  getVariableGroupConfigDiffRows,
  variableGroupConfigSchema,
  type VariableGroupConfig,
} from "#/modules/environment-design/variable-group-config";
import {
  getVolumeConfigDiffRows,
  parseVolumeConfig,
  persistedVolumeConfigSchema,
  type VolumeConfig,
} from "#/modules/environment-design/volume-config";
import type { DiffRow } from "#/modules/services/service-deployment-diff/fields";

export type EnvironmentResourceNodeType = EnvironmentResourceType;
export const isEnvironmentResourceNodeType = isEnvironmentResourceType;

export type EnvironmentResourceNodeConfigByType = {
  variable_group: VariableGroupConfig;
  volume: VolumeConfig;
};

export type DecodedEnvironmentResourceNodeConfig<
  NodeType extends EnvironmentResourceNodeType = EnvironmentResourceNodeType,
> = {
  [TNodeType in NodeType]: {
    nodeType: TNodeType;
    config: EnvironmentResourceNodeConfigByType[TNodeType];
  };
}[NodeType];

export function decodeEnvironmentResourceNodeConfig<Input>(
  nodeType: "variable_group",
  input: Input,
): Effect.Effect<
  DecodedEnvironmentResourceNodeConfig<"variable_group">,
  Schema.SchemaError
>;
export function decodeEnvironmentResourceNodeConfig<Input>(
  nodeType: "volume",
  input: Input,
): Effect.Effect<
  DecodedEnvironmentResourceNodeConfig<"volume">,
  Schema.SchemaError
>;
export function decodeEnvironmentResourceNodeConfig<Input>(
  nodeType: EnvironmentResourceNodeType,
  input: Input,
): Effect.Effect<DecodedEnvironmentResourceNodeConfig, Schema.SchemaError>;
export function decodeEnvironmentResourceNodeConfig<Input>(
  nodeType: EnvironmentResourceNodeType,
  input: Input,
): Effect.Effect<DecodedEnvironmentResourceNodeConfig, Schema.SchemaError> {
  switch (nodeType) {
    case "variable_group":
      return Schema.decodeUnknownEffect(variableGroupConfigSchema)(
        input,
        strictParseOptions,
      ).pipe(Effect.map((config) => ({ nodeType, config })));
    case "volume":
      return Schema.decodeUnknownEffect(persistedVolumeConfigSchema)(
        input,
        strictParseOptions,
      ).pipe(Effect.map((config) => ({ nodeType, config })));
    default: {
      const _exhaustive: never = nodeType;
      return _exhaustive;
    }
  }
}

export function parseEnvironmentResourceNodeConfig<Input>(
  nodeType: "variable_group",
  input: Input,
): VariableGroupConfig;
export function parseEnvironmentResourceNodeConfig<Input>(
  nodeType: "volume",
  input: Input,
): VolumeConfig;
export function parseEnvironmentResourceNodeConfig<Input>(
  nodeType: EnvironmentResourceNodeType,
  input: Input,
): VariableGroupConfig | VolumeConfig;
export function parseEnvironmentResourceNodeConfig<Input>(
  nodeType: EnvironmentResourceNodeType,
  input: Input,
): VariableGroupConfig | VolumeConfig {
  return Effect.runSync(decodeEnvironmentResourceNodeConfig(nodeType, input))
    .config;
}

export function getEnvironmentResourceNodeConfigDiffRows(input: {
  nodeType: "variable_group";
  nodeId: string;
  current: VariableGroupConfig;
  baseline: VariableGroupConfig | null;
}): DiffRow[];
export function getEnvironmentResourceNodeConfigDiffRows(input: {
  nodeType: "volume";
  nodeId: string;
  current: VolumeConfig;
  baseline: VolumeConfig | null;
}): DiffRow[];
export function getEnvironmentResourceNodeConfigDiffRows(input: {
  nodeType: EnvironmentResourceNodeType;
  nodeId: string;
  current: VariableGroupConfig | VolumeConfig;
  baseline: VariableGroupConfig | VolumeConfig | null;
}): DiffRow[];
export function getEnvironmentResourceNodeConfigDiffRows(input: {
  nodeType: EnvironmentResourceNodeType;
  nodeId: string;
  current: VariableGroupConfig | VolumeConfig;
  baseline: VariableGroupConfig | VolumeConfig | null;
}): DiffRow[] {
  switch (input.nodeType) {
    case "variable_group":
      return getVariableGroupConfigDiffRows({
        nodeId: input.nodeId,
        current: decodeStrict(variableGroupConfigSchema, input.current),
        baseline:
          input.baseline === null
            ? null
            : decodeStrict(variableGroupConfigSchema, input.baseline),
      });
    case "volume":
      return getVolumeConfigDiffRows({
        nodeId: input.nodeId,
        current: parseVolumeConfig(input.current),
        baseline:
          input.baseline === null ? null : parseVolumeConfig(input.baseline),
      });
    default: {
      const _exhaustive: never = input.nodeType;
      return _exhaustive;
    }
  }
}

export function getEnvironmentResourceNodeSnapshotResourceName(
  nodeType: EnvironmentResourceNodeType,
) {
  switch (nodeType) {
    case "variable_group":
      return "VariableGroupSnapshot";
    case "volume":
      return "VolumeSnapshot";
    default: {
      const _exhaustive: never = nodeType;
      return _exhaustive;
    }
  }
}
