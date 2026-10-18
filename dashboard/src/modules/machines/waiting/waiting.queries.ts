import { keepPreviousData, queryOptions } from "@tanstack/react-query";
import { listFleetTokensServerFn } from "#/modules/machines/waiting/fleet-token.functions";
import type { EnrollInput } from "#/modules/machines/waiting/server-enrollment";
import { planServerEnrollmentServerFn, serverEnrollmentStatusServerFn, listWaitingServersServerFn, showWaitingServerServerFn } from "#/modules/machines/waiting/waiting.functions";

export const WAITING_POLL_MS = 5_000;

function nextPoll(offset: number) {
  return WAITING_POLL_MS - ((Date.now() - offset) % WAITING_POLL_MS + WAITING_POLL_MS) % WAITING_POLL_MS;
}

export const waitingKeys = { all: ["waiting-servers"] as const };
export const fleetTokenKeys = { all: ["fleet-tokens"] as const };

export function waitingServersQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...waitingKeys.all, organizationSlug] as const,
    queryFn: () => listWaitingServersServerFn({ data: { organizationSlug } }),
    staleTime: WAITING_POLL_MS,
    refetchInterval: () => nextPoll(0),
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
    refetchInterval: () => nextPoll(WAITING_POLL_MS / 2),
  });
}

export function waitingServerDetailQueryOptions(organizationSlug: string, id: string | null) {
  return queryOptions({
    queryKey: ["waiting-server-detail", organizationSlug, id] as const,
    queryFn: () => showWaitingServerServerFn({ data: { organizationSlug, id: id ?? "" } }),
    enabled: id !== null,
    staleTime: WAITING_POLL_MS,
    refetchInterval: WAITING_POLL_MS,
  });
}

export function enrollmentPlanQueryOptions(organizationSlug: string, input: EnrollInput) {
  return queryOptions({
    queryKey: ["server-enrollment-plan", organizationSlug, input] as const,
    queryFn: () => planServerEnrollmentServerFn({ data: { organizationSlug, ...input } }),
    staleTime: 0,
    placeholderData: keepPreviousData,
    retry: false,
  });
}
