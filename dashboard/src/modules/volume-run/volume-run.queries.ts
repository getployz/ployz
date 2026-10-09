import { queryOptions, useQuery } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import { inspectVolumeCopiesServerFn, listActiveVolumeRunsServerFn, listVolumeRunsServerFn } from "./volume-run.functions";

/** A Volume's runs, and the copies they change; the change stream's `volume_run` invalidates this root. */
export const volumeRunKeys = { all: ["volume-run"] as const };

/** A Volume's last 50 runs, newest first. The change stream refetches them when a run's row changes. */
export function volumeRunsQueryOptions(organizationSlug: string, volumeId: string) {
  return queryOptions({
    queryKey: [...volumeRunKeys.all, organizationSlug, volumeId] as const,
    queryFn: () => listVolumeRunsServerFn({ data: { organizationSlug, volumeId } }),
    staleTime: 60_000,
  });
}

export function useVolumeRuns(organizationSlug: string, volumeId: string) {
  return useQuery(volumeRunsQueryOptions(organizationSlug, volumeId));
}

/**
 * Each managed Server's answer for a Volume's copy, the evidence a run plans from. Asked again when a run changes, and
 * every 15 seconds while the panel is open, since a copy can change outside any run.
 */
export function useVolumeMembers(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  return useQuery({
    queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "members"] as const,
    queryFn: () => inspectVolumeCopiesServerFn({ data: { organizationSlug, environment, volumeId } }),
    refetchInterval: 15_000,
  });
}

/** The organization's runs in progress, for the canvas trays. */
export function useActiveVolumeRuns(organizationSlug: string) {
  return useQuery({
    queryKey: [...volumeRunKeys.all, organizationSlug, "active"] as const,
    queryFn: () => listActiveVolumeRunsServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
}
