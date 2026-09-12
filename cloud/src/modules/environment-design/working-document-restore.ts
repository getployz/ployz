import { Schema } from "effect";
import { OrganizationSlug, Uuid } from "./workspace-schemas";
import { environmentSnapshotSourceSchema } from "./environment-snapshot-source";
import { environmentSavedStateBasisSchema } from "./saved-state";

const restoreScope = {
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  revision: Uuid,
};
const nodeCommand = Schema.Struct({
  kind: Schema.Literal("node"), nodeType: Schema.Literals(["service", "variable_group", "volume"]),
  nodeId: Uuid, path: Schema.optionalKey(Schema.NonEmptyString),
});
export const restoreWorkingDocumentSchema = Schema.Union([
  Schema.Struct({ ...restoreScope, snapshotSource: Schema.NullOr(environmentSnapshotSourceSchema),
    command: Schema.Struct({ kind: Schema.Literal("all") }),
  }),
  Schema.Struct({ ...restoreScope, snapshotSource: Schema.NullOr(Schema.Union([
    environmentSnapshotSourceSchema, Schema.Struct({ kind: Schema.Literal("introduction") }),
  ])), command: nodeCommand }),
]);
export type RestoreWorkingDocumentInput = typeof restoreWorkingDocumentSchema.Type;

export const discardEnvironmentChangesSchema = Schema.Struct({
  ...restoreScope,
  savedStateBasis: environmentSavedStateBasisSchema,
  baselineToken: Schema.NonEmptyString,
  command: Schema.Union([Schema.Struct({ kind: Schema.Literal("all") }), nodeCommand]),
});
export type DiscardEnvironmentChangesInput = typeof discardEnvironmentChangesSchema.Type;
