import { useQuery } from "@tanstack/react-query";
import { useMatches } from "@tanstack/react-router";
import {
  createDashboardNavigation,
  getDashboardSectionFromRouteId,
  type DashboardScope,
} from "#/components/dashboard-navigation-model";
import { organizationStateQueryOptions } from "#/modules/environment-design/workspace.queries";

export function useDashboardSection() {
  return useMatches({
    select: (matches) =>
      getDashboardSectionFromRouteId(matches.at(-1)?.routeId),
  });
}

export function useDashboardNavigation(scope: DashboardScope) {
  const section = useDashboardSection();
  // Self-hosted Cloud has no billing, so no Billing destination.
  const billingEnabled =
    useQuery(organizationStateQueryOptions(scope.organizationSlug)).data?.billingEnabled ?? false;
  return createDashboardNavigation(scope, { section, billingEnabled });
}
