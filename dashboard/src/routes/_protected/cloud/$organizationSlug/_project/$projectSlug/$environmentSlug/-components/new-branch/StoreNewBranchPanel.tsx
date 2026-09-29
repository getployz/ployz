import { useState, type ReactNode } from "react";
import { useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import type { BranchPreset, PlannedNode } from "@ployz/sdk";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { Button } from "#/components/ui/button";
import { FieldDescription, FieldGroup, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { branchSetupCommands, ownLineages, type BranchPlan } from "#/modules/config-store/branch-picks";
import { DNS_LABEL_RULE, isDnsLabel } from "#/modules/config-store/store-services";
import { planOf } from "#/modules/config-store/store-branches";
import { branchPlanQuery, environmentsQuery, useBranchPlan, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import type { SetupCommand } from "#/modules/config-store/branch-picks";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { PickingViewProvider, usePickingView, type PickingView } from "./branch-picking";
import { NameSection } from "./NameSection";
import { ServicesSection } from "./ServicesSection";
import { SetupSection } from "./SetupSection";

type Picks = { copy: string[] } | { preset: BranchPreset };
/** The picks of one opening of the panel (`session`), and the Services they are for. */
type PickState = { session: string | null; focus: string[]; picks: Picks };

/**
 * A New branch's picks over the Config Store, shared with the canvas under the panel. Each pick reads the Store's plan
 * (nodes by name); `focus` is the Service the panel opened on.
 */
function useStorePicking(newBranch: { focus: string | null } | null): PickingView | null {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const initialFocus = newBranch?.focus ?? null;
  const session = newBranch ? `open:${initialFocus}` : null;
  const initial: PickState = { session, focus: initialFocus ? [initialFocus] : [], picks: { preset: "only" } };
  const [state, setState] = useState(initial);
  if (state.session !== session) setState(initial);
  const result = useBranchPlan(params.organizationSlug, newBranch ? branchPlanQuery(store, state.focus, state.picks) : null);
  const view = result?.ok ? result.value : null;
  if (!newBranch || !view) return null;
  const plan = planOf(view);
  const byName = new Map(view.nodes.map((node) => [node.name, node]));
  const own = ownLineages(plan);
  const asPlanNode = (node: PlannedNode, role: PlannedNode["role"]): BranchPlan["nodes"][number] => role === "own"
    ? { lineageId: node.name, nodeType: node.kind, role, because: "picked" }
    : { lineageId: node.name, nodeType: node.kind, role };
  return {
    parent: { name: view.from.name },
    plan,
    presets: view.presets.map((preset) => ({ preset })),
    fromPr: new Set(),
    // Copied because something needs it, or used live from further up than the Parent: clicking changes nothing.
    fixed: (node) => (node.role === "own" && node.because !== "picked")
      || (node.role === "live" && (byName.get(node.lineageId)?.owner ?? view.from.name) !== view.from.name),
    liveOwner: (name) => ({ ownsData: byName.get(name)?.data ?? false }),
    ownerName: (name) => byName.get(name)?.owner ?? view.from.name,
    setPreset: (preset) => setState((current) => ({ ...current, picks: { preset } })),
    toggled: (name) => {
      const node = byName.get(name);
      return node && asPlanNode(node, node.toggled);
    },
    toggle: (name) => {
      const on = !own.includes(name);
      setState((current) => ({
        ...current,
        picks: { copy: on ? [...own, name] : own.filter((candidate) => candidate !== name) },
        focus: on ? [...current.focus, name] : current.focus.filter((candidate) => candidate !== name),
      }));
    },
  };
}

/** Holds a New branch's picks over the Config Store while its panel is open. */
export function StorePickingProvider({ newBranch, children }: { newBranch: { focus: string | null } | null; children: ReactNode }) {
  return <PickingViewProvider picking={useStorePicking(newBranch)}>{children}</PickingViewProvider>;
}

/**
 * "New branch of X" over the Config Store: name it, pick what gets an Own Copy, create it (and deploy it), then open its
 * canvas. With `fix`, a failed Deployment, the Branch copies the Services it failed to apply, with that change.
 */
export function StoreNewBranchPanel({ focus, fix }: { focus: string | null; fix: string | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const picking = usePickingView();
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  const listing = useStoreView(params.organizationSlug, environmentsQuery(params.projectSlug));
  const taken = new Set(listing.ok ? listing.value.environments.map((environment) => environment.name) : []);
  const [name, setName] = useState<string | null>(null);
  const [keep, setKeep] = useState(false);
  const [setupCommands, setSetupCommands] = useState<SetupCommand[]>([]);
  const [pending, setPending] = useState<"deploy" | "create" | null>(null);
  if (!picking) return null;

  const { plan, parent } = picking;
  const own = ownLineages(plan);
  const nameOf = (node: string) => node;
  const branchName = (name ?? freeName(fix && focus ? `fix-${focus}` : "new-branch", taken)).trim();
  const nameError = nameProblem(branchName, taken);
  const blocked = own.length === 0 && !fix ? "Pick something to change" : null;

  async function submit(deployNow: boolean) {
    if (blocked || nameError || pending) return;
    const branch = { project: store.project, environment: branchName };
    setPending(deployNow ? "deploy" : "create");
    try {
      // Awaited: Cloud opens the Branch's canvas once the Store has it.
      await writer.commit({
        command: "create_branch", id: crypto.randomUUID(), from: store, name: branch.environment, copy: own, live: [], keep,
        // A Setup Command runs in an Own Copy, and only while the Branch copies data.
        setup: branchSetupCommands(plan, setupCommands).map((setup) => ({ service: setup.lineageId, command: setup.command })),
        fix,
      }).isPersisted.promise;
      // The writer toasts a refused Deploy; the Branch is made either way.
      if (deployNow) writer.commit({ command: "admit", id: crypto.randomUUID(), environment: branch, services: [], version: null, remove: false, accept_volume_loss: [] });
      await navigate(getDashboardDestination({
        kind: "environment", organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, environmentSlug: branch.environment,
      }, "architecture"));
    } catch {
      // The writer toasted the Store's refusal.
    } finally {
      setPending(null);
    }
  }

  return (
    <form className="flex h-full min-h-0 flex-col" onSubmit={(event) => { event.preventDefault(); void submit(true); }}>
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">{fix && focus ? `Fix ${focus} on a branch` : "New branch"}</span>
        <p className="text-sm break-words text-muted-foreground">From {parent.name}{fix ? ", with the change that failed" : null}</p>
      </CanvasInspectorHeader>
      <FieldGroup className="min-h-0 flex-1 gap-8 overflow-y-auto p-4">
        <FieldDescription>A copy of {parent.name} to change things in without touching it.</FieldDescription>
        <NameSection name={branchName} onName={setName} error={nameError} addresses={[]} />
        <ServicesSection picking={picking} nameOf={nameOf} target={branchName || "this branch"} who="This branch" />
        <SetupSection plan={plan} nameOf={nameOf} setupCommands={setupCommands} onSetupCommands={setSetupCommands} />
        <FieldSet>
          <FieldLegend>After saving</FieldLegend>
          <FieldDescription>Otherwise it can be deleted once its changes are saved.</FieldDescription>
          <Item variant="muted" render={<label htmlFor="branch-keep" />}>
            <ItemMedia><Switch id="branch-keep" checked={keep} onCheckedChange={setKeep} /></ItemMedia>
            <ItemContent><ItemTitle>Keep it after saving</ItemTitle></ItemContent>
          </Item>
        </FieldSet>
      </FieldGroup>
      <div className="flex shrink-0 flex-col gap-2 border-t p-4">
        {blocked ? <Button type="submit" disabled>{blocked}</Button> : (
          <div className="flex gap-2">
            <Button type="submit" className="flex-1" disabled={Boolean(nameError) || pending !== null}>
              {pending === "deploy" && <Spinner data-icon="inline-start" />}Create and deploy
            </Button>
            <Button type="button" variant="outline" className="flex-1" disabled={Boolean(nameError) || pending !== null} onClick={() => void submit(false)}>
              {pending === "create" && <Spinner data-icon="inline-start" />}Just create
            </Button>
          </div>
        )}
      </div>
    </form>
  );
}

/** Why `name` can't name a new Environment of this Project, or null. */
function nameProblem(name: string, taken: ReadonlySet<string>) {
  if (!name) return "Name the branch.";
  if (taken.has(name)) return `${name} is taken in this project.`;
  return isDnsLabel(name) ? null : DNS_LABEL_RULE;
}

/** The first free name: `base`, then `base-2`, `base-3`… */
function freeName(base: string, taken: ReadonlySet<string>) {
  for (let n = 1; ; n++) {
    const name = n === 1 ? base : `${base}-${n}`;
    if (!taken.has(name)) return name;
  }
}
