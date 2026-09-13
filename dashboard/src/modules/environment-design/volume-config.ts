import { parseResourceConfig } from "@ployz/sdk/config";
import { sharedSchema } from "#/modules/environment-design/service-config";
import { Schema, SchemaGetter } from "effect";
import { decodeStrict } from "#/modules/environment-design/schema";
import { getResourceDeploymentDiffRows, type DiffRow } from "#/modules/services/service-deployment-diff/fields";

export const volumeConfigSchema = sharedSchema((value) => parseResourceConfig("volume", value));

export type VolumeConfig = typeof volumeConfigSchema.Type;

export function namedVolumeConfig(name: string): VolumeConfig {
  return decodeStrict(volumeConfigSchema, { version: 2, name });
}

const historicalVolumeConfigSchema = Schema.Union([
  Schema.Struct({
    version: Schema.Literal(1),
    name: Schema.String,
  }),
  Schema.Struct({
    version: Schema.Literal(2),
    name: Schema.String,
    storage: Schema.optionalKey(Schema.Unknown),
  }),
]);

export const persistedVolumeConfigSchema = historicalVolumeConfigSchema.pipe(
  Schema.decodeTo(volumeConfigSchema, {
    decode: SchemaGetter.transform((config) => ({
      version: 2,
      name: config.name,
    })),
    encode: SchemaGetter.transform((config) => ({
      version: 2,
      name: config.name,
    })),
  }),
);

export function parseVolumeConfig<Input>(input: Input): VolumeConfig {
  return decodeStrict(persistedVolumeConfigSchema, input);
}

export function getVolumePhysicalName(volumeResourceId: string): string {
  return `vol-${volumeResourceId}`;
}

export function getDeletedDeployedVolumeResourceIds(input: {
  desiredVolumeResourceIds: Iterable<string>;
  appliedVolumeResourceIds: Iterable<string>;
}): string[] {
  const desired = new Set(input.desiredVolumeResourceIds);
  return [...new Set(input.appliedVolumeResourceIds)].filter(
    (id) => !desired.has(id),
  );
}

export function getVolumeConfigDiffRows(input: {
  nodeId: string;
  current: VolumeConfig;
  baseline: VolumeConfig | null;
}): DiffRow[] {
  return getResourceDeploymentDiffRows("volume", input);
}
