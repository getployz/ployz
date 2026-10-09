import { queryOptions, useQuery } from "@tanstack/react-query";
import { listVolumeRunsServerFn } from "./volume-run.functions";

/** A Volume's runs; the change stream's `volume_run` invalidates this root. */
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
