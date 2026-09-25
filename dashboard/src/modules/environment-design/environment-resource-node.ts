import { Effect, Schema } from "effect";
import type { EnvironmentResourceType } from "#/modules/environment-design/environment-resource-types";
import { strictParseOptions } from "#/modules/environment-design/schema";
import {
  persistedVolumeConfigSchema,
  type VolumeConfig,
} from "#/modules/environment-design/volume-config";

export type EnvironmentResourceNodeType = EnvironmentResourceType;

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
