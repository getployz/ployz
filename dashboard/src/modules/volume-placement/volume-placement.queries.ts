import { queryOptions, useQuery } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import { listPlacementsServerFn, regionMovesServerFn } from "./volume-placement.functions";

/** Volume placements; the change stream's `volume_placement` invalidates this root. */
export const volumePlacementKeys = { all: ["volume-placement"] as const };

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

/**
 * The moves the next Deploy makes to bring Volumes into their Services' regions. `version` is the Environment's diff
 * version, so a staged region change reads them again.
 */
export function regionMovesQueryOptions(organizationSlug: string, environment: EnvironmentRef, version: string | null) {
  return queryOptions({
    queryKey: [...volumePlacementKeys.all, organizationSlug, "region", environment.project, environment.environment, version] as const,
    queryFn: () => regionMovesServerFn({ data: { organizationSlug, environment } }),
    staleTime: 60_000,
  });
}
