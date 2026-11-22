/** @data-source remote: a Volume's newest finished mirror round and its newest done one; the change stream refetches it when a run's row changes; a requested Refresh now is polled every 3s by volume-run.queries.ts, and its end refetches this by the volume-run key prefix. Mirror status polls every minute because the background sync writes it without a change to invalidate on */
import { queryOptions, useQuery } from "@tanstack/react-query";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { mirrorStatusesServerFn } from "./volume-copies.functions";

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
