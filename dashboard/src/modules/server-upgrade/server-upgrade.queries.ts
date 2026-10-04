import { queryOptions, useQuery, useSuspenseQuery } from "@tanstack/react-query";
import type { ReleaseChannel } from "./server-upgrade";
import { listLatestServerUpgradesServerFn, readChannelReleaseServerFn } from "./server-upgrade.functions";

/** Each Server's latest attempts; the change stream's `server_upgrade` invalidates this root. */
export const serverUpgradeKeys = { all: ["server-upgrade"] as const };
/** The Release Channel pointers: their own root, so an attempt changing doesn't refetch them. */
const channelReleaseKeys = { all: ["channel-release"] as const };

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

/** A Release Channel pointer: one release line's, or the unscoped stable one the new-major-line notice reads. */
export type ChannelPointer =
  | { readonly channel: ReleaseChannel; readonly line: string }
  | { readonly channel: "stable"; readonly line: null };

/** The newest release `pointer` names. Cloud caches the pointer for a few minutes too. */
export function channelReleaseQueryOptions(pointer: ChannelPointer) {
  return queryOptions({
    queryKey: [...channelReleaseKeys.all, pointer.channel, pointer.line] as const,
    queryFn: () => readChannelReleaseServerFn({ data: pointer }),
    staleTime: 5 * 60_000,
  });
}

/** Every Server's latest attempt. The page's loader prefetches it. */
export function useServerUpgrades(organizationSlug: string) {
  return useSuspenseQuery(latestServerUpgradesQueryOptions(organizationSlug)).data;
}

/** The release `pointer` names, as Query's result: pending, failed, or the release (null with none). Null reads nothing. */
export function useChannelRelease(pointer: ChannelPointer | null) {
  return useQuery({ ...channelReleaseQueryOptions(pointer ?? { channel: "stable", line: null }), enabled: pointer !== null });
}
