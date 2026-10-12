import { queryOptions, useQuery } from "@tanstack/react-query";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { lastMirrorRefreshServerFn } from "./volume-copies.functions";

/** A Volume's newest ended mirror round and newest done one. */
export const lastMirrorRefreshQueryOptions = (organizationSlug: string, volumeId: string, awaiting = false) => queryOptions({
  queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "last-sync"] as const,
  queryFn: () => lastMirrorRefreshServerFn({ data: { organizationSlug, volumeId } }),
  staleTime: 60_000,
  refetchInterval: awaiting ? 5_000 : false,
});

export function useLastMirrorRefresh(organizationSlug: string, volumeId: string, awaiting: boolean) {
  return useQuery(lastMirrorRefreshQueryOptions(organizationSlug, volumeId, awaiting)).data ?? { ended: null, done: null };
}
