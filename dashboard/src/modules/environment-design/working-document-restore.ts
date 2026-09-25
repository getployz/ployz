import { Schema } from "effect";
import { OrganizationSlug, Uuid } from "./workspace-schemas";
import { environmentSavedStateBasisSchema } from "./saved-state";

const restoreScope = {
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  revision: Uuid,
};
const nodeCommand = Schema.Struct({
  kind: Schema.Literal("node"), nodeType: Schema.Literals(["service", "volume"]),
  nodeId: Uuid, path: Schema.optionalKey(Schema.NonEmptyString),
});
export const discardEnvironmentChangesSchema = Schema.Struct({
  ...restoreScope,
  savedStateBasis: environmentSavedStateBasisSchema,
  headToken: Schema.NonEmptyString,
  command: Schema.Union([Schema.Struct({ kind: Schema.Literal("all") }), nodeCommand]),
});
export type DiscardEnvironmentChangesInput = typeof discardEnvironmentChangesSchema.Type;
