import { createContext, use, type ReactNode } from "react";
import type { BranchPlanView, BranchPreset, PlannedNode } from "@ployz/sdk";

/** What the pick rows and canvas cards read and do over the Config Store's plan, whose nodes go by name. */
export type PickingView = {
  parent: { name: string };
  plan: BranchPlanView;
  presets: ReadonlyArray<{ preset: BranchPreset }>;
  fromPr: ReadonlySet<string>;
  fixed: (node: PlannedNode) => boolean;
  setPreset: (preset: BranchPreset) => void;
  toggle: (name: string) => void;
};
const PickingViewContext = createContext<PickingView | null>(null);

/** Shares the picks with the pick rows and the canvas; without any, the outer picks show. */
export function PickingViewProvider({ picking, children }: { picking: PickingView | null; children: ReactNode }) {
  const outer = use(PickingViewContext);
  return <PickingViewContext value={picking ?? outer}>{children}</PickingViewContext>;
}

/** The open panel's picks; null when nothing is being picked. */
export const usePickingView = () => use(PickingViewContext);

export type NodePick = {
  role: PlannedNode["role"];
  /** Words for what the node becomes: "Separate", "New, empty", "PR's code", "production's", "Not included". */
  label: string;
  /** It runs the pull request's code, another copy needs it, or the Parent doesn't own it: clicking changes nothing. */
  fixed: boolean;
  /** A Live Node that mounts a Volume: the Branch would read and write its real data. */
  ownsData: boolean;
  toggle: () => void;
};

/** What `node` becomes in the Branch being picked. */
export function nodePick(picking: PickingView, node: PlannedNode): NodePick {
  return {
    role: node.role,
    label: node.role === "live" ? `${ownerName(picking, node)}'s` : node.role === "left_out" ? "Not included"
      : picking.fromPr.has(node.name) ? "PR's code" : node.kind === "volume" ? "New, empty" : "Separate",
    fixed: picking.fixed(node),
    ownsData: node.role === "live" && node.data,
    toggle: () => picking.toggle(node.name),
  };
}

/** Where a Live Node runs: the nearest Environment up the tree that runs it, else the Parent. */
export const ownerName = (picking: PickingView, node: PlannedNode) => node.owner ?? picking.parent.name;

/** The node as it would be if toggled, for the choice sheet. */
export const toggledNode = (node: PlannedNode): PlannedNode =>
  ({ ...node, role: node.toggled, toggled: node.role, because: node.toggled === "own" ? "picked" : null });

/** What a canvas node becomes in the Branch being picked; null when nothing is being picked or the plan omits it. */
export function useNodePick(name: string | undefined): NodePick | null {
  const picking = usePickingView();
  const node = picking?.plan.nodes.find((candidate) => candidate.name === name);
  return picking && node ? nodePick(picking, node) : null;
}
