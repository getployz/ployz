import type { BranchPlanView, BranchPreset } from "@ployz/sdk";

export const presetTitles = {
  only: "Only what changes",
  uses: "Plus what it uses",
  all: "Everything",
} satisfies Record<BranchPreset, string>;

/** The Branch gets an Own Copy of data, so its Setup Commands ("Then run") apply. */
export const copiesData = (plan: BranchPlanView) => plan.nodes.some((node) => node.role === "own" && node.kind === "volume");

/** The Setup Commands a Branch keeps: none without an Own Copy of data, else each whole one in its own service. */
export function branchSetupCommands(plan: BranchPlanView, commands: ReadonlyArray<{ lineageId: string; command: string }>) {
  if (!copiesData(plan)) return [];
  const own = new Set(plan.nodes.filter((node) => node.role === "own" && node.kind === "service").map((node) => node.name));
  return commands.filter((setup) => setup.command.trim() && own.has(setup.lineageId))
    .map((setup) => ({ lineageId: setup.lineageId, command: setup.command.trim() }));
}

export const ownLineages = (plan: BranchPlanView) => plan.nodes.filter((node) => node.role === "own").map((node) => node.name);

/** A command a Branch runs in one Service once its Own Copy of data exists. */
export type SetupCommand = { lineageId: string; command: string };
