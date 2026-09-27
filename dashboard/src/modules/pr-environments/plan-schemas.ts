import { Schema } from "effect";
import { OrganizationSlug, ProjectSlug, Uuid } from "#/modules/environment-design/workspace-schemas";
import { BranchPicksSchema, SetupCommandSchema } from "#/modules/branches/branch-schemas";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";

/** The whole PR Environments plan for one repository of a project; saving it replaces the last one. */
export const SetPrEnvironmentPlan = Schema.Struct({
  organizationSlug: OrganizationSlug,
  projectSlug: ProjectSlug,
  repositoryId: githubIdSchema,
  enabled: Schema.Boolean,
  startFromEnvironmentId: Schema.NullOr(Uuid),
  picks: BranchPicksSchema,
  setupCommands: Schema.mutable(Schema.Array(SetupCommandSchema)),
  removeOnClose: Schema.Boolean,
  includeBots: Schema.Boolean,
});
export type SetPrEnvironmentPlan = typeof SetPrEnvironmentPlan.Type;

/** The organization whose GitHub App installations to check. */
export const ListPrEnvironmentGrants = Schema.Struct({ organizationSlug: OrganizationSlug });
