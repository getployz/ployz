import { createContext, use, useState, type ReactNode } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { planBranch } from "@ployz/sdk/config";
import { offeredPresets, ownLineages, type BranchPicks, type BranchPlan, type BranchPreset } from "#/modules/branches/branch-plan";
import { useLiveOwner } from "#/modules/branches/use-live-nodes";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * What a New branch panel is picking, shared with the canvas under it so clicking a card toggles its Own Copy. Core plans
 * every pick over the Parent's Working State, with "deployed" meaning the Parent's Applied lineages.
 */
function usePickingState(open: boolean, initialFocus: string | null) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  const applied = useEnvironmentChangeStateProjection({ organizationSlug: params.organizationSlug, environmentId })?.applied.nodes ?? [];
  const { projects, environments } = useWorkspace(params.organizationSlug);
  const ownerOf = useLiveOwner(params.organizationSlug);
  const session = open ? `open:${initialFocus}` : null;
  const [state, setState] = useState<{ session: string | null; focus: string[]; picks: BranchPicks }>(
    { session, focus: initialFocus ? [initialFocus] : [], picks: { preset: "only" } });
  if (state.session !== session) setState({ session, focus: initialFocus ? [initialFocus] : [], picks: { preset: "only" } });

  const parent = findEnvironment(projects, environments, params);
  const intent = document?.intent;
  if (!open || !intent || !parent) return null;
  const owned = new Set([...intent.services.map((node) => node.lineageId), ...intent.volumes.map((node) => node.resourceLineageId)]);
  const focus = state.focus.filter((lineage) => owned.has(lineage));
  const planned = { parent: intent, deployed: applied.map((node) => node.nodeLineageId), focus };
  const plan = planBranch({ ...planned, picks: state.picks });
  const own = ownLineages(plan);
  return {
    parent, intent, plan, focus, picks: state.picks, owned,
    presets: offeredPresets(planned).map((preset) => ({ preset, plan: planBranch({ ...planned, picks: { preset } }) })),
    /** The Environment a Live Node here comes from: the Parent, or the ancestor the Parent itself uses it from. */
    ownerName: (lineage: string) => ownerOf(parent.id, lineage)?.environment.name ?? parent.name,
    setPreset: (preset: BranchPreset) => setState({ ...state, picks: { preset } }),
    toggle(lineage: string) {
      const on = !own.includes(lineage);
      setState({
        session,
        focus: on ? [...focus, lineage] : focus.filter((candidate) => candidate !== lineage),
        picks: { own: on ? [...own, lineage] : own.filter((candidate) => candidate !== lineage) },
      });
    },
  };
}

export type BranchPicking = NonNullable<ReturnType<typeof usePickingState>>;
const BranchPickingContext = createContext<BranchPicking | null>(null);

/** Holds the picks while the New branch panel is open (`open`); `focus` is the lineage the panel opened on. */
export function BranchPickingProvider({ open, focus, children }: { open: boolean; focus: string | null; children: ReactNode }) {
  return <BranchPickingContext value={usePickingState(open, focus)}>{children}</BranchPickingContext>;
}

/** The open New branch panel's picks; null when no panel is open. */
export const useBranchPicking = () => use(BranchPickingContext);

export type NodePick = {
  role: BranchPlan["nodes"][number]["role"];
  /** Words for what the node becomes: "Own copy", "production's, live", "Left out". */
  label: string;
  /** Another copy needs it, or the Parent doesn't own it: clicking changes nothing. */
  fixed: boolean;
  /** A Live Node that mounts a Volume: the Branch would read and write its real data. */
  ownsData: boolean;
  toggle: () => void;
};

/** What a canvas node becomes in the Branch being picked; null when nothing is being picked or the plan omits it. */
export function useNodePick(lineageId: string | undefined): NodePick | null {
  const picking = useBranchPicking();
  const node = picking?.plan.nodes.find((candidate) => candidate.lineageId === lineageId);
  if (!picking || !node) return null;
  const isOwn = node.role === "own";
  return {
    role: node.role,
    label: node.role === "live" ? `${picking.ownerName(node.lineageId)}'s, live` : node.role === "left_out" ? "Left out" : "Own copy",
    fixed: !picking.owned.has(node.lineageId) || (isOwn && node.because !== "picked"),
    ownsData: node.role === "live" && (node.nodeType === "volume" || picking.intent.services.some((service) =>
      service.lineageId === node.lineageId && service.volumeAttachments.length > 0)),
    toggle: () => picking.toggle(node.lineageId),
  };
}
