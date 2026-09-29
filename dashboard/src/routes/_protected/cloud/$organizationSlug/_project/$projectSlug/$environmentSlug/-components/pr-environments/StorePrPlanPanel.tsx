import { useState, type ReactNode } from "react";
import { Link, useLoaderData, useNavigate, useParams } from "@tanstack/react-router";
import type { PrPlan } from "@ployz/sdk";
import { ChevronRightIcon, GitPullRequestIcon, TriangleAlertIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Switch } from "#/components/ui/switch";
import { prPlansQuery, useMissingStorePrGrant } from "#/modules/config-store/store-pull-requests";
import { ownLineages } from "#/modules/config-store/branch-picks";
import { planOf } from "#/modules/config-store/store-branches";
import { branchPlanQuery, environmentSettingsQuery, environmentsQuery, useBranchPlan, useCachedStoreView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_PR_PLAN_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { PickingViewProvider, usePickingView, type PickingView } from "../new-branch/branch-picking";
import { ServicesSection } from "../new-branch/ServicesSection";
import { useSavedSetupCommands } from "../new-branch/SetupCommandsField";
import { SetupSection } from "../new-branch/SetupSection";

type Params = { organizationSlug: string; projectSlug: string; environmentSlug: string };
type PlanChange = Partial<Pick<PrPlan, "enabled" | "start_from" | "copy" | "setup" | "remove_on_close" | "include_bots">>;

/**
 * A repository's PR Environments plan over the Config Store, over its start-from Environment's canvas. Every change
 * saves at once; the switches show it before the Store answers.
 */
export function StorePrPlanPanel({ repositoryId }: { repositoryId: number }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const plans = useStoreView(params.organizationSlug, prPlansQuery(params.projectSlug));
  const saved = plans.ok ? plans.value.plans.find((row) => row.repository_id === repositoryId) : undefined;
  if (!plans.ok || !saved) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <CanvasInspectorHeader params={params}><span className="font-medium">PR environments</span></CanvasInspectorHeader>
        <p className="p-4 text-sm text-muted-foreground">
          {plans.ok ? `No service in ${params.projectSlug} deploys from this repository.` : plans.refusal.message}
        </p>
      </div>
    );
  }
  const prEnvironments = new Set(plans.value.plans.flatMap((plan) => plan.open.map((open) => open.environment)));
  return <Plan params={params} saved={saved} prEnvironments={prEnvironments} />;
}

/** The plan with this tab's unsaved changes over it; a refused change drops out, which is the rollback. */
function usePlanWrite(organizationSlug: string, project: string, saved: PrPlan) {
  const writer = useStoreWriter(organizationSlug);
  const [pending, setPending] = useState<PlanChange>({});
  const plan: PrPlan = { ...saved, ...pending };
  function set(change: PlanChange) {
    setPending((current) => ({ ...current, ...change }));
    const written = writer.commit({
      command: "set_pr_plan", project, repository: saved.repository, enabled: null, start_from: null, copy: null, setup: null,
      remove_on_close: null, include_bots: null, ...change,
    }).isPersisted.promise;
    // Once the Store answered (and its views refetched), the saved plan shows, unless a newer change is pending.
    void written.catch(() => undefined).finally(() => setPending((current) => {
      const next = { ...current };
      // SAFETY: `change` is a PlanChange, so its own keys are PlanChange's.
      for (const key of Object.keys(change) as Array<keyof PlanChange>) if (next[key] === change[key]) delete next[key];
      return next;
    }));
    return written;
  }
  return { plan, set };
}

function Plan({ params, saved, prEnvironments }: { params: Params; saved: PrPlan; prEnvironments: ReadonlySet<string> }) {
  const { organizationSlug, projectSlug } = params;
  const navigate = useNavigate();
  const { plan, set } = usePlanWrite(organizationSlug, projectSlug, saved);
  const approve = useMissingStorePrGrant(organizationSlug, projectSlug, plan.installation_id);
  const listing = useStoreView(organizationSlug, environmentsQuery(projectSlug));
  const environments = listing.ok ? listing.value.environments.filter((environment) => !prEnvironments.has(environment.name)) : [];
  const over = (environmentSlug: string) => void navigate({ to: ENVIRONMENT_PR_PLAN_ROUTE_TO, replace: true,
    params: { ...params, environmentSlug, repositoryId: String(plan.repository_id) } });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">PR environments</span>
        <p className="truncate text-sm text-muted-foreground">{plan.repository}</p>
      </CanvasInspectorHeader>
      <FieldGroup className="min-h-0 flex-1 gap-8 overflow-y-auto p-4">
        {approve && <MissingGrant repository={plan.repository} url={approve} />}
        <div className="flex flex-col items-start gap-3">
          <FieldDescription>Created when a pull request opens. Its merge lands in each environment that deploys the target branch.</FieldDescription>
          <Button variant="outline" onClick={() => void set({ enabled: !plan.enabled })}>
            {plan.enabled ? "Disable PR environments" : "Enable PR environments"}
          </Button>
        </div>
        {plan.enabled && <>
          <FieldSet>
            <FieldLegend>Start from</FieldLegend>
            <FieldDescription>Each pull request starts from this environment's services and variables.</FieldDescription>
            <Field data-invalid={!plan.start_from || undefined}>
              <Select value={plan.start_from} onValueChange={(next) => {
                if (!next || next === plan.start_from) return;
                const from = params.environmentSlug;
                // Refused and rolled back: back over the canvas it was on.
                void set({ start_from: next }).catch(() => over(from));
                over(next);
              }}>
                <SelectTrigger aria-label="Start from" className="w-full" aria-invalid={!plan.start_from || undefined}>
                  <SelectValue placeholder="Pick an environment">{plan.start_from}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {environments.map((environment) => (
                      <SelectItem key={environment.id} value={environment.name} label={environment.name}>{environment.name}</SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
              {!plan.start_from && <FieldError>Pick the environment each pull request starts from.</FieldError>}
            </Field>
          </FieldSet>
          {/* What it copies names the start-from's nodes, which this canvas shows once the panel is over it. */}
          {plan.start_from === params.environmentSlug && <Copies plan={plan} set={set} />}
          {plan.open.length > 0 && (
            <FieldSet>
              <FieldLegend>Open now</FieldLegend>
              <ItemGroup className="gap-2">
                {plan.open.map((open) => (
                  <Item key={open.environment} variant="outline" size="sm"
                    render={<Link to={ENVIRONMENT_INDEX_ROUTE_TO} params={{ ...params, environmentSlug: open.environment }} />}>
                    <ItemMedia variant="icon"><GitPullRequestIcon /></ItemMedia>
                    <ItemContent className="min-w-0">
                      <ItemTitle>{open.environment} <span className="font-normal text-muted-foreground">#{open.number}</span></ItemTitle>
                      {open.title ? <ItemDescription className="truncate">{open.title} · {open.author}</ItemDescription> : null}
                    </ItemContent>
                    <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
                  </Item>
                ))}
              </ItemGroup>
            </FieldSet>
          )}
          <FieldSet>
            <FieldLegend>Closed pull requests</FieldLegend>
            <FieldDescription>Otherwise it's deleted after 7 days without a deploy.</FieldDescription>
            <Item variant="muted" render={<label htmlFor="pr-plan-remove-on-close" />}>
              <ItemMedia><Switch id="pr-plan-remove-on-close" checked={plan.remove_on_close} onCheckedChange={(removeOnClose) => void set({ remove_on_close: removeOnClose })} /></ItemMedia>
              <ItemContent><ItemTitle>Delete when the PR closes</ItemTitle></ItemContent>
            </Item>
          </FieldSet>
          <FieldSet>
            <FieldLegend>Bot PR environments</FieldLegend>
            <FieldDescription>Pull requests from Dependabot, Renovate and other bots.</FieldDescription>
            <Item variant="muted" render={<label htmlFor="pr-plan-bots" />}>
              <ItemMedia><Switch id="pr-plan-bots" checked={plan.include_bots} onCheckedChange={(includeBots) => void set({ include_bots: includeBots })} /></ItemMedia>
              <ItemContent><ItemTitle>Enable bot PR environments</ItemTitle></ItemContent>
            </Item>
          </FieldSet>
        </>}
      </FieldGroup>
    </div>
  );
}

/**
 * What each PR Environment copies from the start-from Environment (the canvas's picks, shared with its cards), and the
 * commands its copied Services run to seed new, empty data.
 */
function Copies({ plan, set }: { plan: PrPlan; set: ReturnType<typeof usePlanWrite>["set"] }) {
  const picking = usePickingView();
  const setup = useSavedSetupCommands(plan.setup.map((command) => ({ lineageId: command.service, command: command.command })),
    (whole) => void set({ setup: whole.map((command) => ({ service: command.lineageId, command: command.command })) }));
  if (!picking) return null;
  const nameOf = (name: string) => name;
  return (
    <>
      <ServicesSection picking={picking} nameOf={nameOf} who="The PR"
        target={<><GitPullRequestIcon aria-hidden="true" className="size-3.5 shrink-0" />each pull request</>} />
      <SetupSection plan={picking.plan} nameOf={nameOf} setupCommands={setup.commands} onSetupCommands={setup.onChange} onSetupBlur={setup.onBlur} />
    </>
  );
}

/**
 * A PR plan's picks over the Config Store while its page is open over the start-from's canvas, shared with the canvas
 * cards: the repository's Services always run the PR's code, and each other toggle saves the plan's copies at once.
 */
function useStorePrPicking(prPlan: { repositoryId: number } | null): PickingView | null {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const writer = useStoreWriter(params.organizationSlug);
  const plans = useCachedStoreView(params.organizationSlug, prPlan ? prPlansQuery(params.projectSlug) : null);
  const settings = useCachedStoreView(params.organizationSlug, prPlan ? environmentSettingsQuery(store) : null);
  const saved = plans?.ok ? plans.value.plans.find((row) => row.repository_id === prPlan?.repositoryId) : undefined;
  const [copy, setCopy] = useState<string[] | null>(null);
  const active = saved?.enabled && saved.start_from === params.environmentSlug ? saved : null;
  const focus = active && settings?.ok ? settings.value.settings.flatMap((row) => {
    const [service, setting] = row.path.split(".");
    return setting === "repository" && row.value === active.repository && service ? [service] : [];
  }) : [];
  const picks = copy ?? active?.copy ?? [];
  const result = useBranchPlan(params.organizationSlug, active ? branchPlanQuery(store, focus, { copy: [...focus, ...picks] }) : null);
  const view = result?.ok ? result.value : null;
  if (!active || !view) return null;
  const plan = planOf(view);
  const byName = new Map(view.nodes.map((node) => [node.name, node]));
  const fromPr = new Set(focus);
  const own = ownLineages(plan).filter((name) => !fromPr.has(name));
  return {
    parent: { name: view.from.name },
    plan,
    presets: [],
    fromPr,
    fixed: (node) => fromPr.has(node.lineageId) || (node.role === "own" && node.because !== "picked")
      || (node.role === "live" && (byName.get(node.lineageId)?.owner ?? view.from.name) !== view.from.name),
    liveOwner: (name) => ({ ownsData: byName.get(name)?.data ?? false }),
    ownerName: (name) => byName.get(name)?.owner ?? view.from.name,
    setPreset: () => undefined,
    toggled: (name) => {
      const node = byName.get(name);
      if (!node) return undefined;
      return node.toggled === "own"
        ? { lineageId: node.name, nodeType: node.kind, role: "own", because: "picked" }
        : { lineageId: node.name, nodeType: node.kind, role: node.toggled };
    },
    toggle: (name) => {
      const next = own.includes(name) ? own.filter((other) => other !== name) : [...own, name];
      setCopy(next);
      // Saved at once; a refusal toasts and the saved copies show again.
      void writer.commit({
        command: "set_pr_plan", project: params.projectSlug, repository: active.repository, enabled: null, start_from: null,
        copy: next, setup: null, remove_on_close: null, include_bots: null,
      }).isPersisted.promise.catch(() => undefined).finally(() => setCopy((current) => current === next ? null : current));
    },
  };
}

/** Holds a PR plan's picks over the Config Store while its page is open. */
export function StorePrPickingProvider({ prPlan, children }: { prPlan: { repositoryId: number } | null; children: ReactNode }) {
  return <PickingViewProvider picking={useStorePrPicking(prPlan)}>{children}</PickingViewProvider>;
}

/** The installation hasn't accepted Pull requests: read and Checks: write, so nothing starts. */
function MissingGrant({ repository, url }: { repository: string; url: string }) {
  return (
    <Item variant="outline" state="warning" size="sm" role="note">
      <ItemMedia><TriangleAlertIcon className="text-warning" /></ItemMedia>
      <ItemContent>
        <ItemDescription className="text-foreground">
          The GitHub App needs Pull requests: read and Checks: write on {repository}.{" "}
          <a href={url} target="_blank" rel="noreferrer" className="underline underline-offset-4">Approve on GitHub</a>
        </ItemDescription>
      </ItemContent>
    </Item>
  );
}
