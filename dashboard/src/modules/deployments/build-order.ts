import type { MachineId } from "@ployz/sdk";
import { Schema } from "effect";
import type { CandidateReason } from "#/modules/deployments/image-build";
import { OrganizationSlug } from "#/modules/environment-design/workspace-schemas";

export const BUILD_ORDERS = ["servers-only", "github-then-servers", "servers-then-github", "github-only"] as const;
export type BuildOrder = (typeof BUILD_ORDERS)[number];

export const BUILD_ORDER_LABELS = {
  "servers-only": "Your servers only",
  "github-then-servers": "GitHub, then your servers",
  "servers-then-github": "Your servers, then GitHub",
  "github-only": "GitHub only",
} satisfies Record<BuildOrder, string>;

/**
 * The Organization's Build Order as the Org Store holds it: one row, keyed by the Organization.
 * `null` until the Organization chooses one; then the default applies.
 */
export type BuildOrderRow = { id: string; buildOrder: BuildOrder | null };

/**
 * The Build Order of an Organization that never chose one: its servers only until GitHub is set up,
 * then GitHub first. GitHub is set up once any repository its Services build from has the build
 * workflow on its default branch, so a GitHub build there can start.
 */
export const defaultBuildOrder = (githubSetUp: boolean): BuildOrder => githubSetUp ? "github-then-servers" : "servers-only";

/**
 * One Builder an Image Build may try, and why it is in the walk. `machineId` is a Preferred Server:
 * the Cluster's first choice.
 */
export type BuildCandidate = { builder: "servers" | "github"; reason: CandidateReason; machineId?: MachineId };

/** The Builders a Build Order tries, in turn. */
const ORDERED_BUILDERS = {
  "servers-only": ["servers"],
  "github-then-servers": ["github", "servers"],
  "servers-then-github": ["servers", "github"],
  "github-only": ["github"],
} satisfies Record<BuildOrder, readonly BuildCandidate["builder"][]>;

/**
 * The Builders one Image Build walks: the Service's Preferred Builder, then the Build Order without
 * that Builder. A Preferred Server stands in for "your servers", with it first.
 */
export const imageBuildWalk = (order: BuildOrder, preferred: "github" | MachineId | undefined): BuildCandidate[] => {
  const walk: BuildCandidate[] = [];
  if (preferred === "github") walk.push({ builder: "github", reason: "preferred" });
  else if (preferred !== undefined) walk.push({ builder: "servers", reason: "preferred", machineId: preferred });
  const rest = ORDERED_BUILDERS[order].filter((builder) => !walk.some((first) => first.builder === builder));
  return [...walk, ...rest.map((builder, index): BuildCandidate => ({ builder, reason: index === 0 ? "first_in_build_order" : "next_in_build_order" }))];
};

export const buildOrderEditSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  buildOrder: Schema.Literals(BUILD_ORDERS),
});
