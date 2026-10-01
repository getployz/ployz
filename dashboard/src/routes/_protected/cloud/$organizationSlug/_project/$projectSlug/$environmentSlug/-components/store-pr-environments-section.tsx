import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { Link } from "@tanstack/react-router";
import type { PrPlan } from "@ployz/sdk";
import { ChevronRightIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { planSummary, prPlansQuery } from "#/modules/config-store/store-pull-requests";
import { useMissingStorePrGrant } from "#/modules/config-store/store-pr-grants.queries";
import { useStoreView } from "#/modules/config-store/store-view.queries";
import { ENVIRONMENT_PR_PLAN_ROUTE_TO } from "./environment-route-paths";

type Place = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/**
 * Settings → Project over the Config Store: one row per GitHub repository the Project deploys from, each opening its
 * plan over its start-from Environment's canvas (this one's until it has one).
 */
export function StorePrEnvironmentsSection(place: Place) {
  const plans = useStoreView(place.organizationSlug, prPlansQuery(place.projectSlug));
  if (!plans.ok || plans.value.plans.length === 0) return null;
  return (
    <SettingsSection id="pr-environments" title="PR environments">
      <ItemGroup className="gap-2">
        {plans.value.plans.map((plan) => <PlanRow key={plan.repository_id} place={place} plan={plan} />)}
      </ItemGroup>
    </SettingsSection>
  );
}

function PlanRow({ place, plan }: { place: Place; plan: PrPlan }) {
  const approve = useMissingStorePrGrant(place.organizationSlug, place.projectSlug, plan.installation_id);
  const params = { ...place, environmentSlug: plan.start_from ?? place.environmentSlug, repositoryId: String(plan.repository_id) };
  return (
    <Item variant="outline" size="sm" render={<Link to={ENVIRONMENT_PR_PLAN_ROUTE_TO} params={params} />}>
      <ItemMedia><GitHubMarkIcon aria-hidden="true" className="size-4" /></ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle className="min-w-0"><span className="truncate">{plan.repository}</span></ItemTitle>
        <ItemDescription>{planSummary(plan)}</ItemDescription>
        {approve && <ItemDescription className="text-warning">Needs permissions approved on GitHub</ItemDescription>}
      </ItemContent>
      <ItemActions>
        {plan.open.length > 0 && <span className="text-sm text-muted-foreground">{plan.open.length} open</span>}
        <ChevronRightIcon className="size-4 text-muted-foreground" />
      </ItemActions>
    </Item>
  );
}
