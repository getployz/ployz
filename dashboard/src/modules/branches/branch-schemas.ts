import { Schema } from "effect";
import { EnvironmentName, OrganizationSlug, Uuid } from "#/modules/environment-design/workspace-schemas";

const Lineages = Schema.mutable(Schema.Array(Uuid));

export const BranchPicksSchema = Schema.Union([
  Schema.Struct({ preset: Schema.Literals(["only", "uses", "all"]) }),
  Schema.Struct({ own: Lineages }),
]);

/** Make a Branch of `parentEnvironmentId`: core re-plans `picks` over `focus` on the server. */
export const CreateBranch = Schema.Struct({
  organizationSlug: OrganizationSlug,
  parentEnvironmentId: Uuid,
  name: EnvironmentName,
  focus: Lineages,
  picks: BranchPicksSchema,
  keep: Schema.Boolean,
});
export type CreateBranch = typeof CreateBranch.Type;
