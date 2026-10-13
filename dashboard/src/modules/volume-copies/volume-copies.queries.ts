import { queryOptions, useQuery } from "@tanstack/react-query";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { lastMirrorRefreshServerFn } from "./volume-copies.functions";

/** A Volume's newest ended mirror round and newest done one. */
export const lastMirrorRefreshQueryOptions = (organizationSlug: string, volumeId: string) => queryOptions({
  queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "last-sync"] as const,
  queryFn: () => lastMirrorRefreshServerFn({ data: { organizationSlug, volumeId } }),
  staleTime: 60_000,
});

export function useLastMirrorRefresh(organizationSlug: string, volumeId: string) {
  return useQuery(lastMirrorRefreshQueryOptions(organizationSlug, volumeId)).data ?? { ended: null, done: null };
}
