import type { BranchPicks, BranchPlan, BranchPreset } from "@ployz/sdk/config";

export type { BranchPicks, BranchPlan, BranchPreset };

type PlanNode = BranchPlan["nodes"][number];

export const presetTitles = {
  only: "Only what changes",
  uses: "Plus what it uses",
  all: "Everything",
} satisfies Record<BranchPreset, string>;

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

/** A command a Branch runs in one Service once its Own Copy of data exists. */
export type SetupCommand = { lineageId: string; command: string };
