/** @data-source remote: a Volume's newest finished mirror round and its newest done one; the change stream refetches it when a run's row changes; a requested Refresh now refetches it when it answers. Mirror status polls every minute because the background sync writes it without a change to invalidate on */
import { queryOptions, useQuery } from "@tanstack/react-query";
import { defineChange } from "#/collections/change-log/declare";
import { mirrorStatusesServerFn } from "./mirror-sync.functions";

/** What the mirror syncs saw of each Volume's mirrors. */
export const mirrorSyncKeys = { all: ["mirror-sync"] as const };

/** A run ending changes what a mirror round saw: its Volume's run row changes with the sync it started. */
export const mirrorSyncChange = defineChange({
  name: "mirror_sync",
  sources: ["volume_run"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: mirrorSyncKeys.all }),
});

/** What the mirror syncs last saw of each of a Volume's mirrors. */
export const mirrorStatusesQueryOptions = (organizationSlug: string, volumeId: string) => queryOptions({
  queryKey: [...mirrorSyncKeys.all, organizationSlug, volumeId, "status"] as const,
  queryFn: () => mirrorStatusesServerFn({ data: { organizationSlug, volumeId } }),
  staleTime: 60_000,
  refetchInterval: 60_000,
});

export function useMirrorStatuses(organizationSlug: string, volumeId: string) {
  return useQuery(mirrorStatusesQueryOptions(organizationSlug, volumeId)).data ?? [];
}
