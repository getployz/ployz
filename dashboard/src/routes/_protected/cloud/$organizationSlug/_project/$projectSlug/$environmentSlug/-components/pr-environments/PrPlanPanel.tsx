import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { ChevronRightIcon, GitPullRequestIcon, TriangleAlertIcon } from "lucide-react";
import { planBranch } from "@ployz/sdk/config";
import { getEnvironmentDeploymentsCollection, type PrEnvironmentPlanRow } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { Badge } from "#/components/ui/badge";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel, FieldLegend, FieldSet, FieldTitle } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { RadioGroup, RadioGroupItem } from "#/components/ui/radio-group";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Switch } from "#/components/ui/switch";
import { listNames, offeredPresets, presetSummary, presetTitles, type BranchPreset } from "#/modules/branches/branch-plan";
import { useEnvironmentChangeStates } from "#/modules/deployments/environment-change-state.queries";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { destinations, trackedBranches } from "#/modules/pr-environments/destinations";
import { usePrEnvironmentPlans, useSetPrEnvironmentPlan } from "#/modules/pr-environments/plan.collection";
import { useMissingPrEnvironmentGrant } from "#/modules/pr-environments/plan.queries";
import { githubAppRepository } from "#/modules/pr-environments/repositories";
import { openPrEnvironments } from "#/modules/pr-environments/pull-request";
import { CanvasInspectorHeader } from "../CanvasInspectorHeader";
import { ENVIRONMENT_PR_PLAN_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

type Workspace = ReturnType<typeof useWorkspace>;

/** A repository's PR Environments plan, over its start-from Environment's canvas. Every change saves at once. */
export function PrPlanPanel({ repositoryId }: { repositoryId: number }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const workspace = useWorkspace(params.organizationSlug);
  const project = workspace.projects.find((row) => row.slug === params.projectSlug);
  return project && <Plan repositoryId={repositoryId} project={project} environments={workspace.environments} branches={workspace.branches} />;
}

function Plan({ repositoryId, project, environments: all, branches }: {
  repositoryId: number;
  project: Workspace["projects"][number];
  environments: Workspace["environments"];
  branches: Workspace["branches"];
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { organizationSlug } = params;
  const navigate = useNavigate();
  const plan = usePrEnvironmentPlans(organizationSlug, project).find((row) => row.repositoryId === repositoryId);
  const save = useSetPrEnvironmentPlan(organizationSlug, project.slug);
  const approve = useMissingPrEnvironmentGrant(organizationSlug, plan?.installationId ?? 0);
  const environments = all.filter((environment) => environment.projectId === project.id)
    .sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime());
  const prEnvironmentIds = new Set(branches.flatMap((branch) => branch.prNumber === null ? [] : [branch.environmentId]));
  const open = openPrEnvironments(branches, project.id, repositoryId).flatMap((branch) => {
    const environment = environments.find((row) => row.id === branch.environmentId);
    return environment ? [{ branch, environment }] : [];
  });
  const { data: attempts } = useLiveSuspenseQuery(getEnvironmentDeploymentsCollection(organizationSlug, useCollectionScope()));
  // The Org Store keeps each Environment's latest attempt, so having none means it was never deployed.
  const deployed = new Set(attempts.map((attempt) => attempt.environmentId));
  const startFrom = environments.find((environment) => environment.id === plan?.startFromEnvironmentId);
  const intent = useEnvironmentDocument(organizationSlug, startFrom?.id ?? null)?.intent;
  const changeStates = useEnvironmentChangeStates(organizationSlug, useCollectionScope());
  const lineageName = useLineageNames(organizationSlug);

  if (!plan) {
    return (
      <div className="flex h-full min-h-0 flex-col">
        <CanvasInspectorHeader params={params}><span className="font-medium">PR environments</span></CanvasInspectorHeader>
        <p className="p-4 text-sm text-muted-foreground">No service in {project.name} deploys from this repository.</p>
      </div>
    );
  }
  const set = (change: Parameters<typeof save>[1]) => save(plan, change);

  // Core plans the presets over the start-from Environment's Working State, with the repository's services changing.
  const applied = changeStates.find((state) => state.environmentId === startFrom?.id)?.applied.nodes ?? [];
  const planned = intent && {
    parent: intent,
    deployed: applied.map((node) => node.nodeLineageId),
    focus: intent.services.filter((node) => githubAppRepository(node.config)?.repositoryId === repositoryId).map((node) => node.lineageId),
  };
  const presets: BranchPreset[] = planned ? offeredPresets(planned) : ["only", "uses", "all"];
  const presetPlan = (preset: BranchPreset) => planned && planBranch({ ...planned, picks: { preset } });
  const only = presetPlan("only");
  const nameOf = (lineage: string) => lineageName(lineage, plan.startFromEnvironmentId ?? "");

  // Where merges land, by the Destinations rule over each Environment's latest Saved State.
  const candidates = environments.map((environment) => ({
    id: environment.id,
    prEnvironment: prEnvironmentIds.has(environment.id),
    savedServices: changeStates.find((state) => state.environmentId === environment.id)?.saved?.nodes
      .flatMap((node) => node.nodeType === "service" ? [node.config] : []) ?? [],
  }));
  const landings = trackedBranches(candidates, repositoryId).map((branch) => `${branch} → ${listNames(
    destinations({ environments: candidates, repositoryId, targetBranch: branch })
      .map((id) => environments.find((environment) => environment.id === id)?.name ?? id))}`);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <span className="font-medium">PR environments</span>
        <p className="truncate text-sm text-muted-foreground">{plan.repository}</p>
      </CanvasInspectorHeader>
      <FieldGroup className="min-h-0 flex-1 overflow-y-auto p-4">
        {approve && <MissingGrant repository={plan.repository} url={approve} />}
        <FieldLabel htmlFor="pr-plan-enabled">
          <Field orientation="horizontal">
            <FieldContent>
              <span className="font-medium">Create an environment for every pull request</span>
              <FieldDescription>Each pull request to {plan.repository} gets its own environment, running its code.</FieldDescription>
            </FieldContent>
            <Switch id="pr-plan-enabled" checked={plan.enabled} onCheckedChange={(enabled) => set({ enabled })} />
          </Field>
        </FieldLabel>
        <Field data-invalid={!startFrom || undefined}>
          <FieldLabel htmlFor="pr-plan-start-from">Start from</FieldLabel>
          <Select value={startFrom?.id ?? null} onValueChange={(next) => {
            const environment = environments.find((candidate) => candidate.id === next);
            if (!environment || environment.id === startFrom?.id) return;
            set({ startFromEnvironmentId: environment.id });
            void navigate({ to: ENVIRONMENT_PR_PLAN_ROUTE_TO, replace: true,
              params: { ...params, environmentSlug: environment.namespace, repositoryId: String(repositoryId) } });
          }}>
            <SelectTrigger id="pr-plan-start-from" className="w-full" aria-invalid={!startFrom || undefined}>
              <SelectValue placeholder="Pick an environment">{startFrom?.name}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {environments.filter((environment) => !prEnvironmentIds.has(environment.id)).map((environment) => (
                  <SelectItem key={environment.id} value={environment.id} label={environment.name}>
                    {environment.name}
                    {!deployed.has(environment.id) && <span className="text-muted-foreground">not deployed</span>}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          {startFrom
            ? <FieldDescription>Services from {plan.repository} run the pull request's code. Everything else starts from {startFrom.name}.</FieldDescription>
            : <FieldError>The environment it started from was torn down. Pick another: no PR environment starts until you do.</FieldError>}
        </Field>
        <FieldSet>
          <FieldLegend>What comes along</FieldLegend>
          <RadioGroup value={"preset" in plan.picks ? plan.picks.preset : null} onValueChange={(value) => {
            const preset = presets.find((candidate) => candidate === value);
            if (preset) set({ picks: { preset } });
          }}>
            {presets.map((preset) => {
              const presetPlanned = presetPlan(preset);
              return (
                <FieldLabel key={preset} htmlFor={`pr-plan-preset-${preset}`}>
                  <Field orientation="horizontal">
                    <RadioGroupItem value={preset} id={`pr-plan-preset-${preset}`} />
                    <FieldContent>
                      <FieldTitle>{presetTitles[preset]}</FieldTitle>
                      {presetPlanned && only && <FieldDescription>{presetSummary(preset, presetPlanned, only, nameOf, startFrom?.name ?? "")}</FieldDescription>}
                    </FieldContent>
                  </Field>
                </FieldLabel>
              );
            })}
          </RadioGroup>
          <FieldDescription>PR environments run one replica of each service.</FieldDescription>
        </FieldSet>
        <FieldSet>
          <FieldLegend>Data</FieldLegend>
          <RadioGroup value="empty">
            <FieldLabel htmlFor="pr-plan-data-empty">
              <Field orientation="horizontal"><RadioGroupItem value="empty" id="pr-plan-data-empty" />Start empty</Field>
            </FieldLabel>
            <FieldLabel htmlFor="pr-plan-data-copy">
              <Field orientation="horizontal" data-disabled="true">
                <RadioGroupItem value="copy" id="pr-plan-data-copy" disabled />
                Copy {startFrom ? `${startFrom.name}'s` : "its"} data<Badge variant="secondary">Soon</Badge>
              </Field>
            </FieldLabel>
          </RadioGroup>
        </FieldSet>
        <FieldSet>
          <FieldLegend>When the pull request…</FieldLegend>
          <FieldLabel htmlFor="pr-plan-remove-on-close">
            <Field orientation="horizontal">
              <FieldContent>
                <span className="font-medium">Closes: remove its environment</span>
                <FieldDescription>
                  {plan.removeOnClose ? "Removed as soon as the pull request closes." : "It stays until 7 days pass without a deploy."}
                </FieldDescription>
              </FieldContent>
              <Switch id="pr-plan-remove-on-close" checked={plan.removeOnClose} onCheckedChange={(removeOnClose) => set({ removeOnClose })} />
            </Field>
          </FieldLabel>
          <FieldDescription>
            Merges: settings move only after someone approves them in the PR environment. They go where the target branch deploys
            {landings.length > 0 ? ` (${landings.join(", ")})` : ""}, in the same deploy as the code.
          </FieldDescription>
        </FieldSet>
        <FieldLabel htmlFor="pr-plan-bots">
          <Field orientation="horizontal">
            <FieldContent>
              <span className="font-medium">Bot pull requests</span>
              <FieldDescription>
                {plan.includeBots ? "Dependabot, Renovate and other bots get environments too." : "Pull requests from Dependabot, Renovate and other bots get none."}
              </FieldDescription>
            </FieldContent>
            <Switch id="pr-plan-bots" checked={plan.includeBots} onCheckedChange={(includeBots) => set({ includeBots })} />
          </Field>
        </FieldLabel>
        {open.length > 0 && (
          <FieldSet>
            <FieldLegend>Open now</FieldLegend>
            <ItemGroup className="gap-2">
              {open.map(({ branch, environment }) => (
                <Item key={environment.id} variant="outline" size="sm" render={
                  <Link to="/cloud/$organizationSlug/$projectSlug/$environmentSlug"
                    params={{ organizationSlug, projectSlug: project.slug, environmentSlug: environment.namespace }} />
                }>
                  <ItemMedia variant="icon"><GitPullRequestIcon /></ItemMedia>
                  <ItemContent className="min-w-0">
                    <ItemTitle>{environment.name} <span className="font-normal text-muted-foreground">#{branch.prNumber}</span></ItemTitle>
                    <ItemDescription className="truncate">{branch.prTitle} · {branch.prAuthor}</ItemDescription>
                  </ItemContent>
                  <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
                </Item>
              ))}
            </ItemGroup>
          </FieldSet>
        )}
      </FieldGroup>
    </div>
  );
}

/** The installation hasn't accepted Pull requests: read and Checks: write, so nothing starts. */
function MissingGrant({ repository, url }: { repository: PrEnvironmentPlanRow["repository"]; url: string }) {
  return (
    <Item variant="outline" state="warning" size="sm" role="note">
      <ItemMedia><TriangleAlertIcon className="text-warning" /></ItemMedia>
      <ItemContent>
        <ItemDescription className="text-foreground">
          The GitHub App's installation for {repository} hasn't accepted Pull requests: read and Checks: write. PR environments
          won't start until it does. <a href={url} target="_blank" rel="noreferrer" className="underline underline-offset-4">Review the permissions on GitHub</a>
        </ItemDescription>
      </ItemContent>
    </Item>
  );
}
