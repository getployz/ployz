import {
  checkBranchName,
  planBranch,
  type BranchPicks,
  type BranchPlan,
  type BranchPreset,
} from "@ployz/sdk/config";
import { createCanonicalEnvironmentNamespace } from "#/modules/environment-design/workspace-schemas";

export type { BranchPicks, BranchPlan, BranchPreset };

type PlanNode = BranchPlan["nodes"][number];

/** Picking can't toggle it: the Parent doesn't own it (`owned`), or another copy needs it. */
export const pickFixed = (node: PlanNode, owned: ReadonlySet<string>) =>
  !owned.has(node.lineageId) || (node.role === "own" && node.because !== "picked");

/** The Branch gets an Own Copy of data, so its Setup Commands ("Then run") apply. */
export const copiesData = (plan: BranchPlan) => plan.nodes.some((node) => node.role === "own" && node.nodeType === "volume");

/** The Setup Commands a Branch keeps: none without an Own Copy of data, else each whole one in its own service. */
export function branchSetupCommands(plan: BranchPlan, commands: ReadonlyArray<{ lineageId: string; command: string }>) {
  if (!copiesData(plan)) return [];
  const own = new Set(plan.nodes.filter((node) => node.role === "own" && node.nodeType === "service").map((node) => node.lineageId));
  return commands.filter((setup) => setup.command.trim() && own.has(setup.lineageId))
    .map((setup) => ({ lineageId: setup.lineageId, command: setup.command.trim() }));
}

export const ownLineages = (plan: BranchPlan) => plan.nodes.filter((node) => node.role === "own").map((node) => node.lineageId);
export const liveLineages = (plan: BranchPlan) => plan.nodes.filter((node) => node.role === "live").map((node) => node.lineageId);

/** The presets to offer: "Plus what it uses" only when it would add something to "Only what changes". */
export function offeredPresets(input: Omit<Parameters<typeof planBranch>[0], "picks">): BranchPreset[] {
  const own = (preset: BranchPreset) => ownLineages(planBranch({ ...input, picks: { preset } })).join();
  return own("uses") === own("only") ? ["only", "all"] : ["only", "uses", "all"];
}

export const branchNamespace = (projectSlug: string, name: string) =>
  createCanonicalEnvironmentNamespace({ projectSlug, environmentName: name });

/** The managed-hostname suffix a Branch appends to its services' prefixes: `-fix-web`. A root has none. */
export const branchHostnameSuffix = (projectSlug: string, namespace: string, isBranch: boolean) =>
  isBranch ? namespace.slice(projectSlug.length) : "";

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

const names = new Intl.ListFormat("en-GB", { type: "conjunction" });
/** "a", "a and b", "a, b and c". */
export const listNames = (list: string[]) => names.format(list);

const namesWith = (plan: BranchPlan, role: PlanNode["role"], nameOf: (lineage: string) => string) =>
  plan.nodes.filter((node) => node.role === role).map((node) => nameOf(node.lineageId));

/** What a preset will do, in words, from its plan and the "Only what changes" plan it extends. */
export function presetSummary(preset: BranchPreset, plan: BranchPlan, only: BranchPlan, nameOf: (lineage: string) => string, parent: string) {
  const own = namesWith(plan, "own", nameOf);
  const live = namesWith(plan, "live", nameOf);
  if (preset === "all") return `A full copy of ${parent}.`;
  if (preset === "uses") {
    const before = new Set(namesWith(only, "own", nameOf));
    const added = own.filter((name) => !before.has(name));
    const copies = `${listNames(added)} ${added.length === 1 ? "gets a copy" : "get copies"} too`;
    return live.length ? `${copies}.` : `${copies}, so nothing touches ${parent}'s data.`;
  }
  if (own.length === 0) return "Pick what changes.";
  const one = own.length === 1;
  const copies = `${listNames(own)} ${one ? "gets its own copy" : "get their own copies"}.`;
  return live.length ? `${copies} What ${one ? "it uses" : "they use"} comes from ${parent}, live.` : copies;
}
