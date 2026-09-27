import { Link } from "@tanstack/react-router";
import { ChevronRightIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import type { PrEnvironmentPlanRow } from "#/collections/collections";
import { useLineageNames } from "#/modules/branches/use-lineage-names";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import type { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { usePrEnvironmentPlans } from "#/modules/pr-environments/plan.collection";
import { useMissingPrEnvironmentGrant } from "#/modules/pr-environments/plan.queries";
import { planSummary, prPlanInput } from "#/modules/pr-environments/repositories";
import { openPrEnvironments } from "#/modules/pr-environments/pull-request";
import { ENVIRONMENT_PR_PLAN_ROUTE_TO } from "./environment-route-paths";

type Workspace = ReturnType<typeof useWorkspace>;

/** Settings → Project: one row per GitHub repository the project deploys from, each opening its plan page. */
export function PrEnvironmentsSection({ organizationSlug, project, environments, branches }: {
  organizationSlug: string;
  project: Workspace["projects"][number];
  environments: Workspace["environments"];
  branches: Workspace["branches"];
}) {
  const plans = usePrEnvironmentPlans(organizationSlug, project);
  if (plans.length === 0) return null;
  return (
    <section aria-labelledby="pr-environments-heading" className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <h2 id="pr-environments-heading" className="text-base font-semibold">PR environments</h2>
        <p className="text-sm text-muted-foreground">
          Each pull request can get its own environment, running its code. It's removed when the pull request closes.
        </p>
      </div>
      <ItemGroup className="gap-2">
        {plans.map((plan) => {
          const startFrom = environments.find((environment) => environment.id === plan.startFromEnvironmentId);
          // Opens over the start-from Environment's canvas; a torn-down one leaves the plan asking, over the Default Environment's.
          const over = startFrom ?? project.resolvedEnvironment;
          return over && (
            <PlanRow key={plan.repositoryId} organizationSlug={organizationSlug} plan={plan} startFrom={startFrom}
              open={openPrEnvironments(branches, project.id, plan.repositoryId).length}
              params={{ organizationSlug, projectSlug: project.slug, environmentSlug: over.namespace, repositoryId: String(plan.repositoryId) }} />
          );
        })}
      </ItemGroup>
    </section>
  );
}

function PlanRow({ organizationSlug, plan, startFrom, open, params }: {
  organizationSlug: string;
  plan: PrEnvironmentPlanRow;
  startFrom: Workspace["environments"][number] | undefined;
  /** How many PR Environments are open for the repository. */
  open: number;
  params: { organizationSlug: string; projectSlug: string; environmentSlug: string; repositoryId: string };
}) {
  const approve = useMissingPrEnvironmentGrant(organizationSlug, plan.installationId);
  const intent = useEnvironmentDocument(organizationSlug, startFrom?.id ?? null)?.intent;
  const applied = useEnvironmentChangeStateProjection({ organizationSlug, environmentId: startFrom?.id ?? "" })?.applied.nodes ?? [];
  const lineageName = useLineageNames(organizationSlug);
  const summary = planSummary(plan, startFrom?.name,
    intent ? prPlanInput(intent, applied.map((node) => node.nodeLineageId), plan.repositoryId, plan.picks) : null,
    (lineage) => lineageName(lineage, startFrom?.id ?? ""));
  return (
    <Item variant="outline" size="sm" render={<Link to={ENVIRONMENT_PR_PLAN_ROUTE_TO} params={params} />}>
      <ItemMedia><GitHubMarkIcon aria-hidden="true" className="size-4" /></ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle className="min-w-0"><span className="truncate">{plan.repository}</span></ItemTitle>
        <ItemDescription>{summary}</ItemDescription>
        {approve && <ItemDescription className="text-warning">Needs permissions approved on GitHub</ItemDescription>}
      </ItemContent>
      <ItemActions>
        {open > 0 && <span className="text-sm text-muted-foreground">{open} open</span>}
        <ChevronRightIcon className="size-4 text-muted-foreground" />
      </ItemActions>
    </Item>
  );
}
