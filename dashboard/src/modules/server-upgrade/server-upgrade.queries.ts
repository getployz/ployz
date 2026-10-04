import { queryOptions, skipToken, useQuery, useSuspenseQuery } from "@tanstack/react-query";
import type { ChannelPointer } from "./server-upgrade";
import { listLatestServerUpgradesServerFn, readChannelReleaseServerFn } from "./server-upgrade.functions";

/** Each Server's latest attempts; the change stream's `server_upgrade` invalidates this root. */
export const serverUpgradeKeys = { all: ["server-upgrade"] as const };
/** The Release Channel pointers: their own root, so an attempt changing doesn't refetch them. */
const channelReleaseKeys = { all: ["channel-release"] as const };

/** Each Server's latest Upgrade attempt, keyed by Machine ID. The change stream refetches it when an attempt changes. */
export function latestServerUpgradesQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...serverUpgradeKeys.all, organizationSlug, "latest"] as const,
    queryFn: () => listLatestServerUpgradesServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
}

/** The newest release `pointer` names; a null pointer reads nothing. Cloud caches the pointer for a few minutes too. */
export function channelReleaseQueryOptions(pointer: ChannelPointer | null) {
  return queryOptions({
    queryKey: [...channelReleaseKeys.all, pointer?.channel ?? null, pointer?.line ?? null] as const,
    queryFn: pointer === null ? skipToken : () => readChannelReleaseServerFn({ data: pointer }),
    staleTime: 5 * 60_000,
  });
}

/** Every Server's latest attempt. The page's loader prefetches it. */
export function useServerUpgrades(organizationSlug: string) {
  return useSuspenseQuery(latestServerUpgradesQueryOptions(organizationSlug)).data;
}

/** The release `pointer` names, as Query's result: pending, failed, or the release (null with none). Null reads nothing. */
export function useChannelRelease(pointer: ChannelPointer | null) {
  return useQuery(channelReleaseQueryOptions(pointer));
}
