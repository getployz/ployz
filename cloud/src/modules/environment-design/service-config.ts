import {
  parseServiceConfig,
  parseServiceSetting,
  type ServiceSettingInput,
} from "@ployz/sdk/config";
import { Effect, Schema, SchemaGetter, SchemaIssue } from "effect";

/** Effect owns the input envelope; Rust owns setting admission and normalization. */
export function sharedSchema<T>(parse: <Input>(value: Input) => T) {
  const decoded = Schema.declare<T>((value): value is T => {
    try { parse(value); return true; } catch { return false; }
  });
  return decoded.pipe(Schema.decodeTo(decoded, {
    decode: SchemaGetter.transformOrFail((value, options) => Effect.try({
      try: () => parse(value),
      catch: (cause) => new SchemaIssue.InvalidValue({
        message: cause instanceof Error ? cause.message : "Invalid service setting",
      }, undefined, options),
    })),
    encode: SchemaGetter.transform((value) => value),
  }));
}

export function serviceFieldSchema<Field extends ServiceSettingInput["field"]>(field: Field) {
  return sharedSchema((value) => parseServiceSetting(field, value));
}

export const sharedServiceConfigSchema = sharedSchema(parseServiceConfig);
export const savedServiceConfigSchema = sharedSchema((value) => {
  const { env: _env, mounts: _mounts, variableGroupAttachments: _variableGroupAttachments, ...settings } = parseServiceConfig(value);
  return settings;
});
