import { createSelectSchema } from "drizzle-orm/effect-schema";
import { Effect, Schema } from "effect";
import {
  environmentCanvasNodePosition,
  environmentResource,
  resourceLineage,
} from "#/modules/environment-design/tables";
import {
  ENVIRONMENT_RESOURCE_TYPES,
} from "#/modules/environment-design/environment-resource-types";
import {
  environmentVariableGroupSelectSchema,
  variableSelectSchema,
} from "#/modules/environment-design/variables";
import {
  OrganizationSlug,
  Uuid,
} from "#/modules/environment-design/workspace-schemas";

const requiredTrimmedString = Schema.Trim.check(Schema.isNonEmpty());
const resourceName = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Resource name is required" }),
  Schema.isMaxLength(64, {
    message: "Resource names must be 64 characters or fewer",
  }),
);

export const environmentResourceTypeSchema = Schema.Literals(
  ENVIRONMENT_RESOURCE_TYPES,
);

const resourceLineageDbSelectSchema = createSelectSchema(resourceLineage, {
  canonicalName: requiredTrimmedString,
  canonicalSlug: requiredTrimmedString,
});

const environmentResourceDbSelectSchema = createSelectSchema(
  environmentResource,
  {
    implementationType: environmentResourceTypeSchema,
  },
);

const resourceLineageSelectSchema = Schema.Struct({
  id: resourceLineageDbSelectSchema.fields.id,
  projectId: resourceLineageDbSelectSchema.fields.projectId,
  canonicalName: resourceLineageDbSelectSchema.fields.canonicalName,
  canonicalSlug: resourceLineageDbSelectSchema.fields.canonicalSlug,
  createdAt: resourceLineageDbSelectSchema.fields.createdAt,
  updatedAt: resourceLineageDbSelectSchema.fields.updatedAt,
});

export const environmentResourceSelectSchema = Schema.Struct({
  id: environmentResourceDbSelectSchema.fields.id,
  projectId: environmentResourceDbSelectSchema.fields.projectId,
  environmentId: environmentResourceDbSelectSchema.fields.environmentId,
  lineageId: environmentResourceDbSelectSchema.fields.lineageId,
  implementationType:
    environmentResourceDbSelectSchema.fields.implementationType,
  variableGroupId: environmentResourceDbSelectSchema.fields.variableGroupId,
  name: resourceName,
  slug: requiredTrimmedString,
  deletedAt: Schema.NullOr(Schema.Date),
  createdAt: environmentResourceDbSelectSchema.fields.createdAt,
  updatedAt: environmentResourceDbSelectSchema.fields.updatedAt,
});

const canvasPositionDbSelectSchema = createSelectSchema(
  environmentCanvasNodePosition,
);

export const environmentResourceCanvasPositionSchema = Schema.Struct({
  id: canvasPositionDbSelectSchema.fields.id,
  environmentId: canvasPositionDbSelectSchema.fields.environmentId,
  resourceType: canvasPositionDbSelectSchema.fields.resourceType,
  resourceId: canvasPositionDbSelectSchema.fields.resourceId,
  x: canvasPositionDbSelectSchema.fields.x,
  y: canvasPositionDbSelectSchema.fields.y,
  createdAt: canvasPositionDbSelectSchema.fields.createdAt,
  updatedAt: canvasPositionDbSelectSchema.fields.updatedAt,
});

const environmentResourceExportSchema = Schema.Struct({
  key: variableSelectSchema.fields.key,
  value: variableSelectSchema.fields.value,
  variableId: variableSelectSchema.fields.id,
});

export const variableGroupResourceRecordSchema = Schema.Struct({
  resource: Schema.Struct({
    ...environmentResourceSelectSchema.fields,
    implementationType: Schema.Literal("variable_group"),
    variableGroupId: Uuid,
  }),
  lineage: resourceLineageSelectSchema,
  variableGroup: environmentVariableGroupSelectSchema,
  canvasPosition: Schema.NullOr(environmentResourceCanvasPositionSchema),
  variables: Schema.mutable(Schema.Array(variableSelectSchema)),
  exports: Schema.mutable(Schema.Array(environmentResourceExportSchema)),
  consumerCount: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)),
  projectSlug: Schema.NonEmptyString,
  environmentSlug: Schema.NonEmptyString,
});

const volumeAttachmentSummarySchema = Schema.Struct({
  serviceId: Uuid,
  mountPath: Schema.NonEmptyString,
});

export const volumeResourceRecordSchema = Schema.Struct({
  resource: Schema.Struct({
    ...environmentResourceSelectSchema.fields,
    implementationType: Schema.Literal("volume"),
    variableGroupId: Schema.Null,
  }),
  lineage: resourceLineageSelectSchema,
  canvasPosition: Schema.NullOr(environmentResourceCanvasPositionSchema),
  attachments: Schema.mutable(Schema.Array(volumeAttachmentSummarySchema)),
  consumerCount: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)),
  runtimeStatus: Schema.NullOr(Schema.NonEmptyString),
  projectSlug: Schema.NonEmptyString,
  environmentSlug: Schema.NonEmptyString,
});

const canvasCoordinates = {
  x: Schema.Finite.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(0)),
  ),
  y: Schema.Finite.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(0)),
  ),
};

export const createVariableGroupResourceSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  name: environmentResourceSelectSchema.fields.name,
  ...canvasCoordinates,
});

export const updateVariableGroupResourceSchema = Schema.Struct({
  revision: Uuid,
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  resourceId: Uuid,
  name: environmentResourceSelectSchema.fields.name,
});

export const deleteVariableGroupResourcePlanSchema = Schema.Struct({
  revision: Uuid,
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  resourceId: Uuid,
});

export const updateEnvironmentResourceCanvasPositionSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  resourceId: Uuid,
  x: Schema.Finite,
  y: Schema.Finite,
});

export const createVolumeResourceSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  name: environmentResourceSelectSchema.fields.name,
  ...canvasCoordinates,
});

export const updateVolumeResourceSchema = Schema.Struct({
  revision: Uuid,
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  resourceId: Uuid,
  name: environmentResourceSelectSchema.fields.name,
});

export const deleteVolumeResourceSchema = Schema.Struct({
  revision: Uuid,
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  resourceId: Uuid,
});

type Mutable<T> = { -readonly [Key in keyof T]: T[Key] };

export type EnvironmentResourceType = typeof environmentResourceTypeSchema.Type;
export type VariableGroupResourceRecord = Mutable<
  typeof variableGroupResourceRecordSchema.Type
>;
export type VolumeResourceRecord = Mutable<
  typeof volumeResourceRecordSchema.Type
>;
export type VolumeAttachmentSummary = typeof volumeAttachmentSummarySchema.Type;
export type CreateVariableGroupResourceInput =
  typeof createVariableGroupResourceSchema.Type;
export type UpdateVariableGroupResourceInput =
  typeof updateVariableGroupResourceSchema.Type;
export type DeleteVariableGroupResourcePlanInput =
  typeof deleteVariableGroupResourcePlanSchema.Type;
export type UpdateEnvironmentResourceCanvasPositionInput =
  typeof updateEnvironmentResourceCanvasPositionSchema.Type;
export type CreateVolumeResourceInput = typeof createVolumeResourceSchema.Type;
export type UpdateVolumeResourceInput = typeof updateVolumeResourceSchema.Type;
export type DeleteVolumeResourceInput = typeof deleteVolumeResourceSchema.Type;
