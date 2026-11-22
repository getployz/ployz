import { queryOptions, useQuery } from "@tanstack/react-query";
import { listPlacementsServerFn } from "./volume-placement.functions";
import { defineChange } from "#/collections/change-log/declare";

/** Volume placements; the change stream's `volume_placement` invalidates this root. */
export const volumePlacementKeys = { all: ["volume-placement"] as const };

export const volumePlacementChange = defineChange({
  name: "volume_placement",
  sources: ["volume_placement"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all }),
});

export function volumePlacementsQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...volumePlacementKeys.all, organizationSlug] as const,
    queryFn: () => listPlacementsServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
}

export function useVolumePlacements(organizationSlug: string) {
  return useQuery(volumePlacementsQueryOptions(organizationSlug));
}
