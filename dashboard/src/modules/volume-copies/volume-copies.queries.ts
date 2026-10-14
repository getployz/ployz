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
