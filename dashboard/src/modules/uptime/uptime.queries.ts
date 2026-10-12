import { queryOptions, useMutation, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import { unreachableHost } from "./uptime";
import type { UptimeRange } from "./uptime-range";
import type { ServiceUptime } from "./uptime-status";
import { readEnvironmentUptimeServerFn, readServiceUptimeServerFn, requestUptimeProbeServerFn } from "./uptime-status.functions";

export type UptimeScope = { readonly organizationSlug: string; readonly environmentId: string };

/** Whether this Cloud runs the uptime checker. Without one nothing reads uptime: no check ever gets a result. */
export function useUptimeEnabled(organizationSlug: string) {
  // Set per deploy, so this read never refetches the Organization state on its own.
  return useQuery({ ...organizationStateQueryOptions(organizationSlug), staleTime: Infinity, select: (state) => state.uptimeEnabled }).data ?? false;
}

/** Every check in the Environment, and when its open incident opened. */
export function environmentUptimeQueryOptions(scope: UptimeScope) {
  return queryOptions({
    queryKey: ["uptime", scope.environmentId, "checks"] as const,
    queryFn: () => readEnvironmentUptimeServerFn({ data: scope }),
    staleTime: 30_000,
    // The checker writes without a Cloud write to invalidate on.
    refetchInterval: 60_000,
  });
}

/** One Service's status, uptime, response time and incidents over `range`, or null without a check. */
export function serviceUptimeQueryOptions(scope: UptimeScope, serviceId: string, range: UptimeRange) {
  return queryOptions({
    queryKey: ["uptime", scope.environmentId, serviceId, range.from, range.to] as const,
    queryFn: () => readServiceUptimeServerFn({ data: { ...scope, serviceId, from: range.from, to: range.to } }),
    staleTime: 30_000,
    // A requested probe answers within seconds, and its round can open or close an incident just after.
    refetchInterval: (query) => query.state.data?.probe ? 2_500 : 60_000,
  });
}

/**
 * Check now: shows the Service's probe pending at once, asks for it, then reads the Environment's uptime again so the
 * answer is polled for. Cloud refusing puts the cached uptime back and toasts its words.
 */
export function useRequestUptimeProbe(scope: UptimeScope, serviceId: string) {
  const queryClient = useQueryClient();
  const serviceKey = ["uptime", scope.environmentId, serviceId] as const;
  return useMutation({
    mutationFn: () => requestUptimeProbeServerFn({ data: { ...scope, serviceId } }),
    onMutate: async () => {
      await queryClient.cancelQueries({ queryKey: serviceKey });
      const previous = queryClient.getQueriesData<ServiceUptime | null>({ queryKey: serviceKey });
      const probe = { requestedAt: new Date().toISOString(), answered: false };
      queryClient.setQueriesData<ServiceUptime | null>({ queryKey: serviceKey }, (uptime) => uptime ? { ...uptime, probe } : uptime);
      return { previous };
    },
    onError: (error, _variables, context) => {
      for (const [key, uptime] of context?.previous ?? []) queryClient.setQueryData(key, uptime);
      toast.error(toErrorMessage(error, "Could not check now"));
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: ["uptime", scope.environmentId] });
    },
  });
}

/** The hostname a Service's open incident is about, or null. Never suspends: a failed read leaves the card as it was. */
export function useUnreachableHost(scope: UptimeScope, serviceId: string) {
  const enabled = useUptimeEnabled(scope.organizationSlug);
  const host = useQuery({ ...environmentUptimeQueryOptions(scope), enabled, select: (checks) => unreachableHost(checks, serviceId) }).data;
  return enabled ? host ?? null : null;
}

export function useServiceUptime(scope: UptimeScope, serviceId: string, range: UptimeRange) {
  return useSuspenseQuery(serviceUptimeQueryOptions(scope, serviceId, range));
}

/** Starts the Monitoring tab's read for `range` before it is shown: on hover of its tab or a range. */
export function usePrefetchServiceUptime(scope: UptimeScope, serviceId: string) {
  const queryClient = useQueryClient();
  const enabled = useUptimeEnabled(scope.organizationSlug);
  return (range: UptimeRange) => { if (enabled) void queryClient.prefetchQuery(serviceUptimeQueryOptions(scope, serviceId, range)); };
}
