import {
  checkBranchName,
  planBranch,
  type BranchPicks,
  type BranchPlan,
  type BranchPreset,
  type SavedEnvironmentIntent as CoreEnvironmentIntent,
} from "@ployz/sdk/config";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { createCanonicalEnvironmentNamespace } from "#/modules/environment-design/workspace-schemas";

export type { BranchPicks, BranchPlan, BranchPreset };

/** What a Branch is planned from: the Parent's Working State, its Applied lineages, and what changes. */
export type BranchPlanInput = {
  parent: SavedEnvironmentIntent;
  deployed: string[];
  focus: string[];
  picks: BranchPicks;
};

/** Core's planBranch; the browser passes the redacted Working State, the server the one with ciphertext. */
export function planBranchOf(input: BranchPlanInput): BranchPlan {
  // SAFETY: core parses the Dashboard's document, variables included, in the same shape.
  return planBranch({ ...input, parent: input.parent as unknown as CoreEnvironmentIntent });
}

export const ownLineages = (plan: BranchPlan) => plan.nodes.filter((node) => node.role === "own").map((node) => node.lineageId);
export const liveLineages = (plan: BranchPlan) => plan.nodes.filter((node) => node.role === "live").map((node) => node.lineageId);

/** The presets to offer: "Plus what it uses" only when it would add something to "Only what changes". */
export function offeredPresets(input: Omit<BranchPlanInput, "picks">): BranchPreset[] {
  const own = (preset: BranchPreset) => ownLineages(planBranchOf({ ...input, picks: { preset } })).join();
  return own("uses") === own("only") ? ["only", "all"] : ["only", "uses", "all"];
}

export const branchNamespace = (projectSlug: string, name: string) =>
  createCanonicalEnvironmentNamespace({ projectSlug, environmentName: name });

/** The managed-hostname suffix a Branch appends to its services' prefixes: `-fix-web`. A root has none. */
export const branchHostnameSuffix = (projectSlug: string, namespace: string) => namespace.slice(projectSlug.length);

/** Why a Branch name can't be used, or null. `taken` holds every namespace in the organization. */
export function branchNameError(projectSlug: string, name: string, taken: ReadonlySet<string>): string | null {
  if (!name.trim()) return "Name the branch.";
  const namespace = branchNamespace(projectSlug, name);
  if (taken.has(namespace)) return `${name.trim()} is taken in this organization.`;
  try {
    checkBranchName(namespace);
    return null;
  } catch (error) {
    const why = error instanceof Error ? error.message.replace(/^projectName: /u, "") : "The name isn't valid.";
    return why.startsWith("Project name is longer") ? "Too long: shorten the name." : `${why}.`;
  }
}

/** The first free, valid name: `base`, then `base-2`, `base-3`… */
export function defaultBranchName(projectSlug: string, base: string, taken: ReadonlySet<string>) {
  for (let n = 1; n < 100; n++) {
    const name = n === 1 ? base : `${base}-${n}`;
    if (branchNameError(projectSlug, name, taken) === null) return name;
  }
  // A base too long for any suffix: the name field shows why.
  return base;
}
