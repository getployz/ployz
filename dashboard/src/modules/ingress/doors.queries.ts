/** @data-source remote: the Cluster's Cloudflare setup and latest switch, prefetched by Settings › Network and refetched by the change stream when a row changes; what the Servers hold now is asked of them live once the section shows, polled every 4s while a switch runs or the tunnel is in use; a switch review is read when its dialog opens and kept no longer */
import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import type { DoorsWant } from "./doors.states";
import { observeClusterIngressServerFn, readClusterIngressServerFn, reviewDoorsSwitchServerFn } from "./doors.functions";
import { defineChange } from "#/collections/change-log/declare";

/** The Cluster's ingress; the change stream's `cluster_ingress` invalidates this root. */
export const clusterIngressKeys = { all: ["cluster-ingress"] as const };

export const clusterIngressChange = defineChange({
  name: "cluster_ingress",
  sources: ["organization_cloudflare_ingress", "ingress_doors_attempt"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: clusterIngressKeys.all }),
});

/**
 * Where the Cluster's names come from and the latest switch. The change stream refetches it when a row changes; a
 * setup that died changes no row when its reservation runs out, so the view is read again while one is held.
 */
export function clusterIngressQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...clusterIngressKeys.all, organizationSlug, "view"] as const,
    queryFn: () => readClusterIngressServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
    refetchInterval: (query) => (query.state.data?.setup?.state === "setting_up" ? 30_000 : false),
  });
}

export const useClusterIngress = (organizationSlug: string) => useSuspenseQuery(clusterIngressQueryOptions(organizationSlug)).data;

/** What the Servers show now: asked of them, so a slow Cluster never holds up the section. Polled while a switch is under way. */
export function clusterIngressObservationQueryOptions(organizationSlug: string, polling: boolean) {
  return queryOptions({
    queryKey: [...clusterIngressKeys.all, organizationSlug, "observed"] as const,
    queryFn: () => observeClusterIngressServerFn({ data: { organizationSlug } }),
    staleTime: 10_000,
    refetchInterval: polling ? 4_000 : false,
  });
}

/** What a switch to `to` moves, as the Servers show it when the review opens. Read afresh each time and kept no longer. */
export function reviewDoorsSwitchQueryOptions(organizationSlug: string, to: DoorsWant) {
  return queryOptions({
    queryKey: [...clusterIngressKeys.all, organizationSlug, "review", to] as const,
    queryFn: () => reviewDoorsSwitchServerFn({ data: { organizationSlug, to } }),
    staleTime: 0,
    gcTime: 0,
  });
}
