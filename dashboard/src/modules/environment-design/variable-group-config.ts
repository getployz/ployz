import { Schema } from "effect";
import type { VariableGroupResourceRecord } from "#/modules/environment-design/resources";
import {
  variableValueSchema,
  encryptedSecretValueSchema,
} from "#/modules/environment-design/variables";
import { decodeStrict } from "#/modules/environment-design/schema";

export const variableGroupConfigSchema = Schema.Struct({
  version: Schema.Literal(1),
  name: Schema.String,
  variables: Schema.mutable(Schema.Array(Schema.Struct({
    key: Schema.String,
    description: Schema.NullOr(Schema.String),
    exported: Schema.Boolean,
    value: Schema.Union([
      Schema.Struct({ type: Schema.Literal("plain"), value: Schema.String }),
      Schema.Struct({
        type: Schema.Literal("sealed"),
        hasValue: Schema.Literal(true),
        fingerprint: Schema.NonEmptyString,
        encryptedValue: Schema.optionalKey(encryptedSecretValueSchema),
      }),
    ]),
  }))),
});
export type VariableGroupConfig = typeof variableGroupConfigSchema.Type;

export function projectVariableGroupConfig(
  resource: VariableGroupResourceRecord,
): VariableGroupConfig {
  return {
    version: 1,
    name: resource.resource.name,
    variables: resource.variables
      .map((variable) => ({
        key: variable.key,
        description: variable.description,
        exported: variable.exported,
        value: decodeStrict(variableValueSchema, variable.value),
      }))
      .sort((left, right) => left.key.localeCompare(right.key)),
  };
}

