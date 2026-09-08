import { parseResourceConfig } from "@ployz/sdk/config";
import { sharedSchema } from "#/modules/environment-design/service-config";
import type { VariableGroupResourceRecord } from "#/modules/environment-design/resources";
import {
  variableValueSchema,
} from "#/modules/environment-design/variables";
import { decodeStrict } from "#/modules/environment-design/schema";
import {
  getResourceDeploymentDiffRows,
  type DiffRow,
} from "#/modules/services/service-deployment-diff/fields";

export const variableGroupConfigSchema = sharedSchema((value) => parseResourceConfig("variable_group", value));
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

export function getVariableGroupConfigDiffRows(input: {
  nodeId: string;
  current: VariableGroupConfig;
  baseline: VariableGroupConfig | null;
}): DiffRow[] {
  return getResourceDeploymentDiffRows("variable_group", input);
}
