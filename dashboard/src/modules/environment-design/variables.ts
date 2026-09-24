import { createSelectSchema } from "drizzle-orm/effect-schema";
import { Effect, Schema, SchemaGetter } from "effect";
import {
  VARIABLE_VALUE_KINDS,
  variable,
} from "#/modules/environment-design/tables";
import {
  OrganizationSlug,
  Uuid,
} from "#/modules/environment-design/workspace-schemas";

const variableName = Schema.Trim.pipe(
  Schema.check(
    Schema.isNonEmpty({ message: "Variable key is required" }),
    Schema.isMaxLength(128, {
      message: "Variable keys must be 128 characters or fewer",
    }),
    Schema.isPattern(/^[A-Za-z_][A-Za-z0-9_]*$/, {
      message:
        "Variable keys must start with a letter or underscore and only include letters, numbers, and underscores",
    }),
  ),
  Schema.decode({
    decode: SchemaGetter.transform((value) => value.toUpperCase()),
    encode: SchemaGetter.transform((value) => value.toUpperCase()),
  }),
);

const nullableDescription = Schema.NullOr(
  Schema.Trim.check(Schema.isMaxLength(500)),
);

export const variableValueKindSchema = Schema.Literals(VARIABLE_VALUE_KINDS);

export const encryptedSecretValueSchema = Schema.Struct({
  version: Schema.Literal(1),
  iv: Schema.String,
  tag: Schema.String,
  ciphertext: Schema.String,
});

const valuePartOwnerSchema = Schema.Union([
  Schema.Struct({ scope: Schema.Literal("self") }),
  Schema.Struct({
    scope: Schema.Literal("service"),
    lineageId: Uuid,
  }),
]);

export const variableValuePartsSchema = Schema.Array(
  Schema.Union([
    Schema.Struct({ kind: Schema.Literal("text"), value: Schema.String }),
    Schema.Struct({
      kind: Schema.Literal("ref"),
      owner: valuePartOwnerSchema,
      key: variableName,
    }),
  ]),
);

const variablePlainValueInputSchema = Schema.Struct({
  type: Schema.Literal("plain"),
  value: Schema.String,
});

const variableSealedValueInputSchema = Schema.Struct({
  type: Schema.Literal("sealed"),
  value: Schema.NonEmptyString.annotate({
    message: "A sealed value is required",
  }),
});

export const variableValueInputSchema = Schema.Union([
  variablePlainValueInputSchema,
  variableSealedValueInputSchema,
]);

const variablePlainValueSchema = Schema.Struct({
  type: Schema.Literal("plain"),
  value: Schema.String,
});

const variableSealedValueSchema = Schema.Struct({
  type: Schema.Literal("sealed"),
  hasValue: Schema.Literal(true),
  fingerprint: Schema.NonEmptyString,
});

export const variableValueSchema = Schema.Union([
  variablePlainValueSchema,
  variableSealedValueSchema,
]);

const variableDbSelectSchema = createSelectSchema(variable);

export const variableSelectSchema = Schema.Struct({
  unresolvedReferences: Schema.optionalKey(Schema.Array(Schema.String)),
  id: variableDbSelectSchema.fields.id,
  serviceId: variableDbSelectSchema.fields.serviceId,
  key: variableName,
  description: nullableDescription,
  exported: Schema.Boolean,
  value: variableValueSchema,
  createdAt: variableDbSelectSchema.fields.createdAt,
  updatedAt: Schema.Date,
});

const variableCreateFields = {
  id: Schema.optionalKey(Uuid),
  key: variableSelectSchema.fields.key,
  description: variableSelectSchema.fields.description.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(null)),
  ),
  exported: variableSelectSchema.fields.exported.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(false)),
  ),
  value: variableValueInputSchema,
};

export const createServiceVariableSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  revision: Uuid,
  environmentId: Uuid,
  serviceId: Uuid,
  ...variableCreateFields,
});

const variableUpdateFields = {
  variableId: Uuid,
  key: variableSelectSchema.fields.key,
  description: variableSelectSchema.fields.description,
  exported: variableSelectSchema.fields.exported,
  value: variableValueInputSchema,
};

export const updateServiceVariableSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  revision: Uuid,
  environmentId: Uuid,
  serviceId: Uuid,
  ...variableUpdateFields,
});

export const updateServiceVariableExportSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  revision: Uuid,
  environmentId: Uuid,
  serviceId: Uuid,
  variableId: Uuid,
  exported: variableSelectSchema.fields.exported,
});

export const deleteServiceVariableSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  revision: Uuid,
  environmentId: Uuid,
  serviceId: Uuid,
  variableId: Uuid,
});

const bulkServiceVariableCreateSchema = Schema.Struct(variableCreateFields);

const bulkServiceVariableUpdateSchema = Schema.Struct({
  variableId: Uuid,
  key: variableSelectSchema.fields.key,
  value: variableValueInputSchema,
});

export const bulkUpdateServiceVariablesSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  revision: Uuid,
  environmentId: Uuid,
  serviceId: Uuid,
  creates: Schema.Array(bulkServiceVariableCreateSchema),
  updates: Schema.Array(bulkServiceVariableUpdateSchema),
  deletes: Schema.Array(Uuid),
});

type Mutable<T> = { -readonly [Key in keyof T]: T[Key] };

export type VariableRecord = Mutable<typeof variableSelectSchema.Type>;
export type VariableValueInput = typeof variableValueInputSchema.Type;
export type CreateServiceVariableInput = typeof createServiceVariableSchema.Type;
export type UpdateServiceVariableInput = typeof updateServiceVariableSchema.Type;
export type UpdateServiceVariableExportInput =
  typeof updateServiceVariableExportSchema.Type;
export type BulkUpdateServiceVariablesInput =
  typeof bulkUpdateServiceVariablesSchema.Type;
