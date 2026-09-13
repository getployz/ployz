import { useMatches } from "@tanstack/react-router";
import { getDashboardSectionFromRouteId } from "#/components/dashboard-navigation-model";

export function useDashboardSection() {
  return useMatches({
    select: (matches) =>
      getDashboardSectionFromRouteId(matches.at(-1)?.routeId),
  });
}
