import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { observeClusterIngressServerFn, readClusterIngressServerFn } from "./doors.functions";

/** The Cluster's ingress; the change stream's `cluster_ingress` invalidates this root. */
export const clusterIngressKeys = { all: ["cluster-ingress"] as const };

/** Where the Cluster's names come from and the latest switch. The change stream refetches it when a row changes. */
export function clusterIngressQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...clusterIngressKeys.all, organizationSlug, "view"] as const,
    queryFn: () => readClusterIngressServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
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
