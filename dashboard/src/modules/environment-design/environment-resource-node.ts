import { Effect, Schema } from "effect";
import {
  isEnvironmentResourceType,
  type EnvironmentResourceType,
} from "#/modules/environment-design/environment-resource-types";
import { strictParseOptions } from "#/modules/environment-design/schema";
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
  nodeType: EnvironmentResourceNodeType,
  input: Input,
): Effect.Effect<DecodedEnvironmentResourceNodeConfig, Schema.SchemaError>;
export function decodeEnvironmentResourceNodeConfig<Input>(
  nodeType: EnvironmentResourceNodeType,
  input: Input,
): Effect.Effect<DecodedEnvironmentResourceNodeConfig, Schema.SchemaError> {
  switch (nodeType) {
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

export function getEnvironmentResourceNodeConfigDiffRows(input: {
  nodeType: EnvironmentResourceNodeType;
  nodeId: string;
  current: VolumeConfig;
  baseline: VolumeConfig | null;
}): DiffRow[] {
  switch (input.nodeType) {
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
    case "volume":
      return "VolumeSnapshot";
    default: {
      const _exhaustive: never = nodeType;
      return _exhaustive;
    }
  }
}
