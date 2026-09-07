import { Schema } from "effect";
import type { VariableGroupResourceRecord } from "#/modules/environment-design/resources";
import {
  encryptedSecretValueSchema,
  variableValueSchema,
} from "#/modules/environment-design/variables";
import { decodeStrict } from "#/modules/environment-design/schema";
import { areDeepEqual } from "#/utils/schema-path";
import {
  getDiffKind,
  type DiffRow,
} from "#/modules/services/service-deployment-diff/fields";

const variableGroupConfigVariableSchema = Schema.Struct({
  key: Schema.String,
  description: Schema.NullOr(Schema.String),
  exported: Schema.Boolean,
  value: Schema.Union([
    Schema.Struct({
      type: Schema.Literal("plain"),
      value: Schema.String,
    }),
    Schema.Struct({
      type: Schema.Literal("sealed"),
      hasValue: Schema.Literal(true),
      fingerprint: Schema.NonEmptyString,
      encryptedValue: Schema.optionalKey(encryptedSecretValueSchema),
    }),
  ]),
});

export const variableGroupConfigSchema = Schema.Struct({
  version: Schema.Literal(1),
  name: Schema.String,
  variables: Schema.mutable(Schema.Array(variableGroupConfigVariableSchema)),
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

function getVariableDisplayValue(
  variable: VariableGroupConfig["variables"][number] | undefined,
) {
  if (!variable) return "";
  if (variable.value.type === "sealed") return "Secret value";
  return variable.value.value;
}

function getComparableVariable(
  variable: VariableGroupConfig["variables"][number] | undefined,
) {
  if (!variable) return undefined;
  return {
    description: variable.description,
    exported: variable.exported,
    value:
      variable.value.type === "sealed"
        ? {
            type: variable.value.type,
            fingerprint: variable.value.fingerprint,
          }
        : variable.value,
  };
}

function rowsByKey(variables: VariableGroupConfig["variables"]) {
  return new Map(variables.map((variable) => [variable.key, variable]));
}

export function getVariableGroupConfigDiffRows(input: {
  nodeId: string;
  current: VariableGroupConfig;
  baseline: VariableGroupConfig | null;
}): DiffRow[] {
  const rows: DiffRow[] = [];

  if (!input.baseline) {
    rows.push({
      changeKey: `${input.nodeId}:node`,
      label: "Variable Group",
      kind: "add",
      path: "node",
      currentValue: "",
      newValue: input.current.name,
      canDiscard: true,
    });
  }

  if (input.baseline && input.current.name !== input.baseline.name) {
    rows.push({
      changeKey: `${input.nodeId}:name`,
      label: "Name",
      kind: "update",
      path: "name",
      currentValue: input.baseline.name,
      newValue: input.current.name,
      canDiscard: false,
    });
  }

  const baselineVariables = rowsByKey(input.baseline?.variables ?? []);
  const currentVariables = rowsByKey(input.current.variables);
  const keys = [
    ...new Set([...baselineVariables.keys(), ...currentVariables.keys()]),
  ].sort();

  for (const key of keys) {
    const baselineValue = baselineVariables.get(key);
    const currentValue = currentVariables.get(key);
    if (
      areDeepEqual(
        getComparableVariable(baselineValue),
        getComparableVariable(currentValue),
      )
    ) {
      continue;
    }

    rows.push({
      changeKey: `${input.nodeId}:variables.${key}`,
      label: `Variable ${key}`,
      kind: getDiffKind(baselineValue, currentValue),
      path: `variables.${key}`,
      currentValue: getVariableDisplayValue(baselineValue),
      newValue: getVariableDisplayValue(currentValue),
      canDiscard: false,
    });
  }

  return rows;
}
