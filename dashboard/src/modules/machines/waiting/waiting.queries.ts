import { queryOptions } from "@tanstack/react-query";
import { listFleetTokensServerFn } from "#/modules/machines/waiting/fleet-token.functions";
import type { EnrollInput } from "#/modules/machines/waiting/server-enrollment";
import { planServerEnrollmentServerFn, serverEnrollmentStatusServerFn, listWaitingServersServerFn } from "#/modules/machines/waiting/waiting.functions";

export const WAITING_POLL_MS = 5_000;

export const waitingKeys = { all: ["waiting-servers"] as const };
export const fleetTokenKeys = { all: ["fleet-tokens"] as const };

export function waitingServersQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...waitingKeys.all, organizationSlug] as const,
    queryFn: () => listWaitingServersServerFn({ data: { organizationSlug } }),
    staleTime: WAITING_POLL_MS,
    refetchInterval: WAITING_POLL_MS,
  });
}

export function fleetTokensQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...fleetTokenKeys.all, organizationSlug] as const,
    queryFn: () => listFleetTokensServerFn({ data: { organizationSlug } }),
    staleTime: WAITING_POLL_MS,
    refetchInterval: WAITING_POLL_MS,
  });
}

export const enrollmentKeys = { all: ["server-enrollments"] as const };

export function serverEnrollmentsQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...enrollmentKeys.all, organizationSlug] as const,
    queryFn: () => serverEnrollmentStatusServerFn({ data: { organizationSlug } }),
    staleTime: WAITING_POLL_MS,
    refetchInterval: WAITING_POLL_MS,
  });
}

export function enrollmentPlanQueryOptions(organizationSlug: string, input: EnrollInput) {
  return queryOptions({
    queryKey: ["server-enrollment-plan", organizationSlug, input] as const,
    queryFn: () => planServerEnrollmentServerFn({ data: { organizationSlug, ...input } }),
    staleTime: 0,
    refetchInterval: WAITING_POLL_MS,
    retry: false,
  });
}
