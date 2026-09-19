import {
  parseServiceConfig,
  parseServiceSetting,
  type ServiceSettingInput,
  type ServiceConfig,
} from "@ployz/sdk/config";
import { Effect, Schema, SchemaGetter, SchemaIssue } from "effect";

import { encryptedSecretValueSchema, variableValuePartsSchema } from "./variables";
import { Uuid, decodeStrict, type DeepMutable } from "./schema";

const envSourceSchema = Schema.Struct({
  kind: Schema.Literal("variable_group"), resourceId: Uuid, resourceName: Schema.NonEmptyString,
  variableGroupId: Uuid, key: Schema.NonEmptyString,
});
const envValueSchema = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("literal"), value: Schema.String,
    source: Schema.optionalKey(envSourceSchema), parts: Schema.optionalKey(variableValuePartsSchema) }),
  Schema.Struct({ kind: Schema.Literal("secret"), variableId: Schema.optionalKey(Uuid),
    encryptedValue: Schema.optionalKey(encryptedSecretValueSchema), fingerprint: Schema.NonEmptyString,
    source: Schema.optionalKey(envSourceSchema), interpolated: Schema.optionalKey(Schema.Boolean) }),
]);
const attachmentsSchema = Schema.Array(Schema.Struct({ variableGroupId: Uuid, sortOrder: Schema.Int }));
export type DashboardServiceConfig = Omit<ServiceConfig, "env"> & {
  env: Record<string, DeepMutable<typeof envValueSchema.Type>>;
  variableGroupAttachments: Array<{ variableGroupId: string; sortOrder: number }>;
};

/** Core compares ordinary Service values; group expressions remain Dashboard display values here. */
export function toCoreServiceConfig(value: DashboardServiceConfig): ServiceConfig {
  const { variableGroupAttachments: _attachments, env, ...config } = value;
  const coreEnv: ServiceConfig["env"] = {};
  for (const [key, entry] of Object.entries(env)) {
    const { source: _source, ...plain } = entry;
    if (plain.kind === "secret") { coreEnv[key] = plain; continue; }
    const literal: Extract<ServiceConfig["env"][string], { kind: "literal" }> = { kind: "literal", value: plain.value };
    if (plain.parts) {
      const parts: NonNullable<typeof literal.parts> = [];
      for (const part of plain.parts) {
        if (part.kind === "text") parts.push(part);
        else if (part.owner.scope === "self") parts.push({ ...part, owner: { scope: "self" } });
        else if (part.owner.scope === "service") parts.push({ ...part, owner: { scope: "service", lineageId: part.owner.lineageId } });
      }
      // Dashboard compares mixed templates separately; Core never receives group owners.
      if (parts.length === plain.parts.length) literal.parts = parts;
    }
    coreEnv[key] = literal;
  }
  return { ...config, env: coreEnv };
}

/** Validate Dashboard extensions separately from Core-owned settings. */
export function parseDashboardServiceConfig<Input>(value: Input): DashboardServiceConfig {
  const { variableGroupAttachments = [], env = {}, ...settings } = decodeStrict(Schema.Record(Schema.String, Schema.Unknown), value);
  // SAFETY: strict schema admission established the shape; cloning permits mutable domain ownership.
  const attachments = structuredClone(decodeStrict(attachmentsSchema, variableGroupAttachments)) as DashboardServiceConfig["variableGroupAttachments"];
  // SAFETY: strict schema admission established the shape; cloning permits mutable domain ownership.
  const parsedEnv = structuredClone(decodeStrict(Schema.Record(Schema.String, envValueSchema), env)) as DashboardServiceConfig["env"];
  const config = parseServiceConfig({ ...settings, env: {} });
  return { ...config, env: parsedEnv, variableGroupAttachments: attachments };
}

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

export const sharedServiceConfigSchema = sharedSchema(parseDashboardServiceConfig);
export const savedServiceConfigSchema = sharedSchema((value) => {
  const { env: _env, mounts: _mounts, variableGroupAttachments: _variableGroupAttachments, ...settings } = parseDashboardServiceConfig(value);
  return settings;
});
