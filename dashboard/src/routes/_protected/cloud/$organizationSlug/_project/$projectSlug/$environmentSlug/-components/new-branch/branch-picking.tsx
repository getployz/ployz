import { createContext, use, type ReactNode } from "react";
import type { BranchPlan, BranchPreset } from "#/modules/config-store/branch-picks";

type PlanNode = BranchPlan["nodes"][number];

/** What the pick rows and canvas cards read and do over the Config Store's plan, whose nodes go by name. */
export type PickingView = {
  parent: { name: string };
  plan: BranchPlan;
  presets: ReadonlyArray<{ preset: BranchPreset }>;
  fromPr: ReadonlySet<string>;
  fixed: (node: PlanNode) => boolean;
  liveOwner: (lineage: string) => { ownsData: boolean } | null | undefined;
  ownerName: (lineage: string) => string;
  setPreset: (preset: BranchPreset) => void;
  /** The node's role if it were toggled, for the choice sheet. */
  toggled: (lineage: string) => PlanNode | undefined;
  toggle: (lineage: string) => void;
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
  role: PlanNode["role"];
  /** Words for what the node becomes: "Separate", "New, empty", "PR's code", "production's", "Not included". */
  label: string;
  /** It runs the pull request's code, another copy needs it, or the Parent doesn't own it: clicking changes nothing. */
  fixed: boolean;
  /** A Live Node that mounts a Volume: the Branch would read and write its real data. */
  ownsData: boolean;
  toggle: () => void;
};

/** What `node` becomes in the Branch being picked. */
export function nodePick(picking: PickingView, node: PlanNode): NodePick {
  return {
    role: node.role,
    label: node.role === "live" ? `${picking.ownerName(node.lineageId)}'s` : node.role === "left_out" ? "Not included"
      : picking.fromPr.has(node.lineageId) ? "PR's code" : node.nodeType === "volume" ? "New, empty" : "Separate",
    fixed: picking.fixed(node),
    ownsData: node.role === "live" && (picking.liveOwner(node.lineageId)?.ownsData ?? node.nodeType === "volume"),
    toggle: () => picking.toggle(node.lineageId),
  };
}

/** What a canvas node becomes in the Branch being picked; null when nothing is being picked or the plan omits it. */
export function useNodePick(lineageId: string | undefined): NodePick | null {
  const picking = usePickingView();
  const node = picking?.plan.nodes.find((candidate) => candidate.lineageId === lineageId);
  return picking && node ? nodePick(picking, node) : null;
}
