import { createContext, use, useState, type ReactNode } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { planBranch } from "@ployz/sdk/config";
import { offeredPresets, ownLineages, pickFixed, type BranchPicks, type BranchPlan, type BranchPreset } from "#/modules/branches/branch-plan";
import { useLiveOwner } from "#/modules/branches/use-live-nodes";
import { usePrEnvironmentPlan, useSetPrEnvironmentPlan } from "#/modules/pr-environments/plan.collection";
import { prPlanInput } from "#/modules/pr-environments/repositories";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

/**
 * What is being picked over the route's Environment, shared with the canvas under the panel so clicking a card toggles its
 * Own Copy. Core plans every pick over the Environment's Working State, with "deployed" meaning its Applied lineages.
 * A New branch keeps its picks here until created; a PR Environments plan (`prPlan`, over its start-from Environment)
 * keeps its repository's services as Own Copies and saves every pick at once.
 */
function usePickingState(newBranch: { focus: string | null } | null, prPlan: { repositoryId: number } | null) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const document = useEnvironmentDocument(params.organizationSlug, environmentId);
  const applied = useEnvironmentChangeStateProjection({ organizationSlug: params.organizationSlug, environmentId })?.applied.nodes ?? [];
  const { projects, environments } = useWorkspace(params.organizationSlug);
  const ownerOf = useLiveOwner(params.organizationSlug);
  const planRow = usePrEnvironmentPlan(params.organizationSlug, params.projectSlug, prPlan?.repositoryId ?? null);
  const savePlan = useSetPrEnvironmentPlan(params.organizationSlug, params.projectSlug);
  const initialFocus = newBranch?.focus ?? null;
  const session = newBranch ? `open:${initialFocus}` : null;
  const [state, setState] = useState<{ session: string | null; focus: string[]; picks: BranchPicks }>(
    { session, focus: initialFocus ? [initialFocus] : [], picks: { preset: "only" } });
  if (state.session !== session) setState({ session, focus: initialFocus ? [initialFocus] : [], picks: { preset: "only" } });

  const parent = findEnvironment(projects, environments, params);
  const intent = document?.intent;
  // A plan picks only over the Environment it starts from; a torn-down one asks for another first.
  const plan = prPlan && planRow?.startFromEnvironmentId === environmentId ? planRow : null;
  if (!intent || !parent || !(newBranch || plan)) return null;
  const owned = new Set([...intent.services.map((node) => node.lineageId), ...intent.volumes.map((node) => node.resourceLineageId)]);
  const deployed = applied.map((node) => node.nodeLineageId);
  const planned = plan ? prPlanInput(intent, deployed, plan.repositoryId, plan.picks)
    : { parent: intent, deployed, focus: state.focus.filter((lineage) => owned.has(lineage)), picks: state.picks };
  const { focus, picks } = planned;
  const branch = planBranch(planned);
  const own = ownLineages(branch);
  /** The repository's services in a plan: Own Copies running the pull request's code, never unpicked. */
  const fromPr = new Set(plan ? focus : []);
  const setPicks = (next: BranchPicks, nextFocus: string[]) => plan ? savePlan(plan, { picks: next }) : setState({ session, focus: nextFocus, picks: next });
  return {
    parent, intent, plan: branch, focus, picks, fromPr,
    presets: offeredPresets(planned).map((preset) => ({ preset, plan: planBranch({ ...planned, picks: { preset } }) })),
    /** Clicking changes nothing: it runs the pull request's code, the Parent doesn't own it, or another copy needs it. */
    fixed: (node: BranchPlan["nodes"][number]) => fromPr.has(node.lineageId) || pickFixed(node, owned),
    /** Where a Live Node here runs: the Parent, or the ancestor the Parent itself uses it from; null when none does. */
    liveOwner: (lineage: string) => ownerOf(parent.id, lineage),
    /** The Environment a Live Node here comes from. */
    ownerName: (lineage: string) => ownerOf(parent.id, lineage)?.environment.name ?? parent.name,
    setPreset: (preset: BranchPreset) => setPicks({ preset }, focus),
    toggle(lineage: string) {
      const on = !own.includes(lineage);
      setPicks({ own: on ? [...own, lineage] : own.filter((candidate) => candidate !== lineage) },
        on ? [...focus, lineage] : focus.filter((candidate) => candidate !== lineage));
    },
  };
}

export type BranchPicking = NonNullable<ReturnType<typeof usePickingState>>;
const BranchPickingContext = createContext<BranchPicking | null>(null);

/** Holds the picks while a New branch panel (opened on `newBranch.focus`) or a PR Environments plan page is open. */
export function BranchPickingProvider({ newBranch, prPlan, children }: {
  newBranch: { focus: string | null } | null;
  prPlan: { repositoryId: number } | null;
  children: ReactNode;
}) {
  return <BranchPickingContext value={usePickingState(newBranch, prPlan)}>{children}</BranchPickingContext>;
}

/** The open panel's picks; null when nothing is being picked. */
export const useBranchPicking = () => use(BranchPickingContext);

export type NodePick = {
  role: BranchPlan["nodes"][number]["role"];
  /** Words for what the node becomes: "Own copy", "Own copy · from the PR", "production's, live", "Left out". */
  label: string;
  /** It runs the pull request's code, another copy needs it, or the Parent doesn't own it: clicking changes nothing. */
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
  return {
    role: node.role,
    label: node.role === "live" ? `${picking.ownerName(node.lineageId)}'s, live` : node.role === "left_out" ? "Left out"
      : picking.fromPr.has(node.lineageId) ? "Own copy · from the PR" : "Own copy",
    fixed: picking.fixed(node),
    ownsData: node.role === "live" && (picking.liveOwner(node.lineageId)?.ownsData ?? node.nodeType === "volume"),
    toggle: () => picking.toggle(node.lineageId),
  };
}
