import { useMatches } from "@tanstack/react-router";
import { getDashboardSectionFromRouteId } from "#/components/dashboard-navigation-model";

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
