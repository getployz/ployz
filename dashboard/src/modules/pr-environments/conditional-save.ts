import { Schema } from "effect";
import { MergePickSchema } from "#/modules/branches/branch-schemas";
import { OrganizationSlug, Uuid } from "#/modules/environment-design/workspace-schemas";

/**
 * A Conditional Save stands while its PR Environment's Working State and its pull request's target Git branch are what
 * they were at approval: any settings change there withdraws it, new commits don't, and edits in the Destination never do.
 */
export const standing = (
  save: { prEnvironmentId: string | null; workingRevision: string; targetBranch: string },
  prEnvironment: { id: string; revision: string; targetBranch: string } | null | undefined,
) => !!prEnvironment && save.prEnvironmentId === prEnvironment.id
  && save.workingRevision === prEnvironment.revision && save.targetBranch === prEnvironment.targetBranch;

const HeldOn = {
  organizationSlug: OrganizationSlug,
  prEnvironmentId: Uuid,
  destinationEnvironmentId: Uuid,
};

/** Approve the ticked rows of a PR Environment's Goes to section; `review` is the review string the user saw. */
export const ApproveConditionalSave = Schema.Struct({
  ...HeldOn,
  review: Schema.String,
  picks: Schema.mutable(Schema.Array(MergePickSchema)),
});
export type ApproveConditionalSave = typeof ApproveConditionalSave.Type;

/** Withdraw the approval (Undo). */
export const WithdrawConditionalSave = Schema.Struct(HeldOn);
export type WithdrawConditionalSave = typeof WithdrawConditionalSave.Type;

/** A new value an approved row still lacks; stored with the approval without withdrawing it. */
export const GiveConditionalSaveValue = Schema.Struct({
  ...HeldOn,
  key: Schema.String,
  value: Schema.String.check(Schema.isNonEmpty()),
});
export type GiveConditionalSaveValue = typeof GiveConditionalSaveValue.Type;
