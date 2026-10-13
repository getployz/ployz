import { useEffect, useEffectEvent, useState } from "react";
import { queryOptions, useQuery, useQueryClient } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import { ACTIVE_VOLUME_RUN_STATES, type VolumeRunView } from "./volume-run";
import { getVolumeRunServerFn, inspectVolumeCopiesServerFn, listActiveVolumeRunsServerFn } from "./volume-run.functions";

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

const ended = (run: VolumeRunView | undefined) => run !== undefined && !ACTIVE_VOLUME_RUN_STATES.some((state) => state === run.state);

/**
 * Polls the run `watch` names until it ends, then hands it to `onEnd` once and refreshes every run read.
 * `onEnd` gets null when the run still cannot be read after two retries.
 */
export function useWatchVolumeRun(organizationSlug: string, onEnd: (run: VolumeRunView | null) => void) {
  const queryClient = useQueryClient();
  const [id, setId] = useState<string | null>(null);
  const { data: run, isError } = useQuery({
    queryKey: [...volumeRunKeys.all, organizationSlug, "run", id] as const,
    queryFn: () => getVolumeRunServerFn({ data: { organizationSlug, id: id ?? "" } }),
    enabled: id !== null,
    staleTime: 0,
    retry: 2,
    retryDelay: 1_000,
    refetchInterval: (query) => (ended(query.state.data) ? false : 3_000),
  });
  const end = useEffectEvent(onEnd);
  useEffect(() => {
    const last = run?.id === id && ended(run) ? run : null;
    if (id === null || (last === null && !isError)) return;
    setId(null);
    void queryClient.invalidateQueries({ queryKey: volumeRunKeys.all });
    end(last);
  }, [id, run, isError, queryClient]);
  return { watch: setId, watching: id !== null };
}
