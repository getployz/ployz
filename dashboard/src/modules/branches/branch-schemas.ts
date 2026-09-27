import { Schema } from "effect";
import { EnvironmentName, OrganizationSlug, Uuid } from "#/modules/environment-design/workspace-schemas";

const Lineages = Schema.mutable(Schema.Array(Uuid));

export const BranchPicksSchema = Schema.Union([
  Schema.Struct({ preset: Schema.Literals(["only", "uses", "all"]) }),
  Schema.Struct({ own: Lineages }),
]);

/** Core lowers each as one `/bin/sh -c` argument and refuses one past 2000 characters. */
export const SetupCommandSchema = Schema.Struct({
  lineageId: Uuid,
  command: Schema.Trim.check(Schema.isNonEmpty(), Schema.isMaxLength(2000)),
});
const SetupCommands = Schema.mutable(Schema.Array(SetupCommandSchema));

/** Make a Branch of `parentEnvironmentId`: core re-plans `picks` over `focus` on the server. */
export const CreateBranch = Schema.Struct({
  organizationSlug: OrganizationSlug,
  parentEnvironmentId: Uuid,
  name: EnvironmentName,
  focus: Lineages,
  picks: BranchPicksSchema,
  keep: Schema.Boolean,
  setupCommands: SetupCommands,
});
export type CreateBranch = typeof CreateBranch.Type;

/** The Setup Commands that prefill every new Branch of `environmentId`. */
export const SetBranchSetupDefaults = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  setupCommands: SetupCommands,
});
export type SetBranchSetupDefaults = typeof SetBranchSetupDefaults.Type;
