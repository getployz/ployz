import { Schema } from "effect";
import { OrganizationSlug, ProjectSlug, Uuid } from "#/modules/environment-design/workspace-schemas";
import { BranchPicksSchema, SetupCommandSchema } from "#/modules/branches/branch-schemas";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";

/** A change to the PR Environments plan for one repository of a project: only the fields given change. */
export const SetPrEnvironmentPlan = Schema.Struct({
  organizationSlug: OrganizationSlug,
  projectSlug: ProjectSlug,
  repositoryId: githubIdSchema,
  enabled: Schema.optionalKey(Schema.Boolean),
  startFromEnvironmentId: Schema.optionalKey(Schema.NullOr(Uuid)),
  picks: Schema.optionalKey(BranchPicksSchema),
  setupCommands: Schema.optionalKey(Schema.mutable(Schema.Array(SetupCommandSchema))),
  removeOnClose: Schema.optionalKey(Schema.Boolean),
  includeBots: Schema.optionalKey(Schema.Boolean),
});
export type SetPrEnvironmentPlan = typeof SetPrEnvironmentPlan.Type;

/** The organization whose GitHub App installations to check. */
export const ListPrEnvironmentGrants = Schema.Struct({ organizationSlug: OrganizationSlug });
