import { queryOptions, useQuery } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import { inspectVolumeCopiesServerFn, listActiveVolumeRunsServerFn } from "./volume-run.functions";

/** A Volume's runs, and the copies they change; the change stream's `volume_run` invalidates this root. */
export const volumeRunKeys = { all: ["volume-run"] as const };

/**
 * Each managed Server's answer for a Volume's copy, the evidence a run plans from. Asked again when a run changes, and
 * every 15 seconds while the panel is open, since a copy can change outside any run.
 */
export function volumeMembersQueryOptions(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  return queryOptions({
    queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "members"] as const,
    queryFn: () => inspectVolumeCopiesServerFn({ data: { organizationSlug, environment, volumeId } }),
    staleTime: 15_000,
    refetchInterval: 15_000,
  });
}

export function useVolumeMembers(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  return useQuery(volumeMembersQueryOptions(organizationSlug, environment, volumeId));
}

/** The organization's runs in progress, for the canvas trays. */
export function useActiveVolumeRuns(organizationSlug: string) {
  return useQuery({
    queryKey: [...volumeRunKeys.all, organizationSlug, "active"] as const,
    queryFn: () => listActiveVolumeRunsServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
}
