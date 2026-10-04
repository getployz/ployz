import { queryOptions, useQuery } from "@tanstack/react-query";
import type { ReleaseChannel } from "./server-upgrade";
import { listLatestServerUpgradesServerFn, readChannelReleaseServerFn } from "./server-upgrade.functions";

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

/**
 * The newest release on `channel` for release line `line` (`v0`); a null line reads the unscoped pointer, which may
 * name a newer line. Cloud caches the pointer for a few minutes too.
 */
export function channelReleaseQueryOptions(channel: ReleaseChannel, line: string | null) {
  return queryOptions({
    queryKey: [...serverUpgradeKeys.all, "release", channel, line] as const,
    queryFn: () => readChannelReleaseServerFn({ data: { channel, line } }),
    staleTime: 5 * 60_000,
  });
}

/** Every Server's latest attempt; undefined until Cloud has answered (the loader prefetches it). */
export function useServerUpgrades(organizationSlug: string) {
  return useQuery(latestServerUpgradesQueryOptions(organizationSlug)).data;
}

/** Null until Cloud has read it, or when it can't be read. `enabled` false reads nothing. */
export function useChannelRelease(channel: ReleaseChannel, line: string | null, enabled = true) {
  return useQuery({ ...channelReleaseQueryOptions(channel, line), enabled }).data ?? null;
}
