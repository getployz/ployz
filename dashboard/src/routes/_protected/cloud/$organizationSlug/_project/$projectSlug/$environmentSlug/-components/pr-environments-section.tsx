import { Link } from "@tanstack/react-router";
import { ChevronRightIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import type { PrEnvironmentPlanRow } from "#/collections/collections";
import { presetTitles } from "#/modules/branches/branch-plan";
import type { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { usePrEnvironmentPlans } from "#/modules/pr-environments/plan.collection";
import { useMissingPrEnvironmentGrant } from "#/modules/pr-environments/plan.queries";
import { ENVIRONMENT_PR_PLAN_ROUTE_TO } from "./environment-route-paths";

type Workspace = ReturnType<typeof useWorkspace>;

/** A plan in short: "Off", or "On · from staging · Only what changes". */
export function planSummary(plan: PrEnvironmentPlanRow, startFrom: string | undefined) {
  if (!plan.enabled) return "Off";
  if (!startFrom) return "On · pick an environment to start from";
  return `On · from ${startFrom} · ${"preset" in plan.picks ? presetTitles[plan.picks.preset] : "Picked by hand"}`;
}

/** Settings → Project: one row per GitHub repository the project deploys from, each opening its plan page. */
export function PrEnvironmentsSection({ organizationSlug, project, environments }: {
  organizationSlug: string;
  project: Workspace["projects"][number];
  environments: Workspace["environments"];
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
            <PlanRow key={plan.repositoryId} organizationSlug={organizationSlug} plan={plan} summary={planSummary(plan, startFrom?.name)}
              params={{ organizationSlug, projectSlug: project.slug, environmentSlug: over.namespace, repositoryId: String(plan.repositoryId) }} />
          );
        })}
      </ItemGroup>
    </section>
  );
}

function PlanRow({ organizationSlug, plan, summary, params }: {
  organizationSlug: string;
  plan: PrEnvironmentPlanRow;
  summary: string;
  params: { organizationSlug: string; projectSlug: string; environmentSlug: string; repositoryId: string };
}) {
  const approve = useMissingPrEnvironmentGrant(organizationSlug, plan.installationId);
  return (
    <Item variant="outline" size="sm" render={<Link to={ENVIRONMENT_PR_PLAN_ROUTE_TO} params={params} />}>
      <ItemMedia><GitHubMarkIcon aria-hidden="true" className="size-4" /></ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle className="min-w-0"><span className="truncate">{plan.repository}</span></ItemTitle>
        <ItemDescription>{summary}</ItemDescription>
        {approve && <ItemDescription className="text-warning">Needs permissions approved on GitHub</ItemDescription>}
      </ItemContent>
      <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
    </Item>
  );
}
