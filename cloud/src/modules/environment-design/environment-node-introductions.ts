import { Schema } from "effect";
import { variableGroupConfigSchema } from "#/modules/environment-design/variable-group-config";
import { persistedVolumeConfigSchema } from "#/modules/environment-design/volume-config";
import { serviceDeploymentConfigSchema } from "#/modules/environment-design/services";
import { Uuid } from "#/modules/environment-design/workspace-schemas";

const introductionBaseFields = {
  organizationId: Uuid,
  environmentId: Uuid,
  nodeId: Uuid,
  nodeLineageId: Uuid,
  configVersion: Schema.Literal(1),
  createdAt: Schema.Date,
  updatedAt: Schema.Date,
};

export const environmentNodeIntroductionSchema = Schema.Union([
  Schema.Struct({
    ...introductionBaseFields,
    nodeType: Schema.Literal("service"),
    config: serviceDeploymentConfigSchema,
  }),
  Schema.Struct({
    ...introductionBaseFields,
    nodeType: Schema.Literal("variable_group"),
    config: variableGroupConfigSchema,
  }),
  Schema.Struct({
    ...introductionBaseFields,
    nodeType: Schema.Literal("volume"),
    configVersion: Schema.Literal(2),
    config: persistedVolumeConfigSchema,
  }),
]);

export type EnvironmentNodeIntroduction =
  typeof environmentNodeIntroductionSchema.Type;
