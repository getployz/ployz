import { useQuery } from "@tanstack/react-query";
import { useMatches, useSearch } from "@tanstack/react-router";
import {
  canvasRouteId,
  createDashboardNavigation,
  getDashboardSectionFromRouteId,
  type DashboardScope,
} from "#/components/dashboard-navigation-model";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";

export function useDashboardSection() {
  return useMatches({
    select: (matches) =>
      getDashboardSectionFromRouteId(matches.at(-1)?.routeId),
  });
}

/** The deepest route's crumb, if it declares one in `staticData`. */
export function useRouteCrumb() {
  return useMatches({ select: (matches) => matches.at(-1)?.staticData.crumb });
}

export function useDashboardNavigation(scope: DashboardScope) {
  const section = useDashboardSection();
  const search = useSearch({ strict: false });
  // Self-hosted Cloud has no billing, so no Billing destination.
  const billingEnabled =
    useQuery(organizationStateQueryOptions(scope.organizationSlug)).data?.billingEnabled ?? false;
  return createDashboardNavigation(scope, { section, search: { scope: search.scope, section: search.section }, billingEnabled });
}

/** Whether the canvas shows: on Architecture or under a panel over it. */
export function useCanvasShowing() {
  return useMatches({ select: (matches) => matches.some((match) => match.routeId === canvasRouteId) });
}
