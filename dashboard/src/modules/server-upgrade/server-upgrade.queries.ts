import { queryOptions, useQuery } from "@tanstack/react-query";
import { listLatestServerUpgradesServerFn, readStableReleaseServerFn } from "./server-upgrade.functions";

export const serverUpgradeKeys = { all: ["server-upgrade"] as const };

/**
 * Each Server's latest Upgrade attempt, keyed by Machine ID, and when the latest successful one ended. The change
 * stream refetches it when an attempt changes.
 */
export function latestServerUpgradesQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...serverUpgradeKeys.all, organizationSlug, "latest"] as const,
    queryFn: () => listLatestServerUpgradesServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
}

/** The newest stable release on release line `line` (`v0`); Cloud caches the pointer for a few minutes too. */
export function stableReleaseQueryOptions(line: string | null) {
  return queryOptions({
    queryKey: [...serverUpgradeKeys.all, "stable", line] as const,
    queryFn: () => readStableReleaseServerFn({ data: { line: line ?? "" } }),
    staleTime: 5 * 60_000,
    enabled: line !== null,
  });
}

/** The Server's latest attempt, null with none; undefined until Cloud has answered (the loader prefetches it). */
export function useLatestServerUpgrade(organizationSlug: string, machineId: string) {
  const data = useServerUpgrades(organizationSlug);
  return data === undefined ? undefined : data.servers[machineId] ?? null;
}

/** Every Server's latest attempt; undefined until Cloud has answered (the loader prefetches it). */
export function useServerUpgrades(organizationSlug: string) {
  return useQuery(latestServerUpgradesQueryOptions(organizationSlug)).data;
}

/** Null until Cloud has read it, or when it can't be read. */
export function useStableRelease(line: string | null) {
  return useQuery(stableReleaseQueryOptions(line)).data ?? null;
}
