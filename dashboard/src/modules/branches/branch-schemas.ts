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
  /** Fix it on a branch: this failed attempt's Saved configuration of this service stands in for the Parent's. */
  fix: Schema.optional(Schema.Struct({ deploymentId: Uuid, serviceId: Uuid })),
  /** Off makes a starting point: the Branch is written and nothing is admitted. */
  deployNow: Schema.Boolean,
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

/** Update: stage the Parent's deployed changes in the Branch at `revision`. */
export const UpdateBranch = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  revision: Uuid,
});
export type UpdateBranch = typeof UpdateBranch.Type;

/** Own Copy: turn the Live Node `lineageId` into an Own Copy in the Branch at `revision`, from the Environment that runs it. */
export const MakeOwnCopy = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  revision: Uuid,
  lineageId: Uuid,
});
export type MakeOwnCopy = typeof MakeOwnCopy.Type;

/** A kept Branch stays after merging and never closes for being idle. */
export const SetBranchKept = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  kept: Schema.Boolean,
});
export type SetBranchKept = typeof SetBranchKept.Type;

export const CloseBranch = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
});
export type CloseBranch = typeof CloseBranch.Type;

/**
 * One ticked merge row. A variable row names its option; `value` is a new value in plain text ("" for none), sealed on
 * the server when the row is a secret.
 */
export const MergePickSchema = Schema.Struct({
  key: Schema.String,
  option: Schema.optional(Schema.Literals(["from", "parent", "new", "leave_out"])),
  value: Schema.String,
});
export type MergePick = typeof MergePickSchema.Type;

/** Stage the ticked rows of a Branch's review in its Destination; `review` is the review string the user saw. */
export const MergeBranch = Schema.Struct({
  organizationSlug: OrganizationSlug,
  branchEnvironmentId: Uuid,
  destinationRevision: Uuid,
  review: Schema.String,
  picks: Schema.mutable(Schema.Array(MergePickSchema)),
  thenClose: Schema.Boolean,
});
export type MergeBranch = typeof MergeBranch.Type;
