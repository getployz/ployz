/** @data-source remote: a Volume's newest finished mirror round and its newest done one; it sits under the volume-run key, so the change stream refetches it when a run's row changes and a run's end or a requested Refresh now refetches it too. Mirror status polls every minute because the background sync writes it without a change to invalidate on */
import { queryOptions, useQuery } from "@tanstack/react-query";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { mirrorStatusesServerFn } from "./mirror-sync.functions";

/** What the mirror syncs last saw of each of a Volume's mirrors. */
export const mirrorStatusesQueryOptions = (organizationSlug: string, volumeId: string) => queryOptions({
  queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "mirror-status"] as const,
  queryFn: () => mirrorStatusesServerFn({ data: { organizationSlug, volumeId } }),
  staleTime: 60_000,
  refetchInterval: 60_000,
});

export function useMirrorStatuses(organizationSlug: string, volumeId: string) {
  return useQuery(mirrorStatusesQueryOptions(organizationSlug, volumeId)).data ?? [];
}
