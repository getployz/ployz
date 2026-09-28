import { Schema } from "effect";
import { SavePickSchema } from "#/modules/branches/branch-schemas";
import { OrganizationSlug, Uuid } from "#/modules/environment-design/workspace-schemas";

/**
 * A Conditional Save stands while its PR Environment's Working State and its pull request's target Git branch are what
 * they were when saved: any settings change there withdraws it, new commits don't, and edits in the Destination never do.
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

/** Save the kept rows of a PR Environment's changes for one Destination; `review` is the review string the sheet showed. */
export const SaveConditionalSave = Schema.Struct({
  ...HeldOn,
  review: Schema.String,
  picks: Schema.mutable(Schema.Array(SavePickSchema)),
});
export type SaveConditionalSave = typeof SaveConditionalSave.Type;

/** Withdraw the save (Undo). */
export const WithdrawConditionalSave = Schema.Struct(HeldOn);
export type WithdrawConditionalSave = typeof WithdrawConditionalSave.Type;

/** Use a pull request's value that landed only as a hint beside the Destination's own undeployed edit. */
export const TakePullRequestValue = Schema.Struct({
  organizationSlug: OrganizationSlug,
  conditionalSaveId: Uuid,
  key: Schema.String,
});
export type TakePullRequestValue = typeof TakePullRequestValue.Type;
