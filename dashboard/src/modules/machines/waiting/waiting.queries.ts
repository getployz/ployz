import { queryOptions } from "@tanstack/react-query";
import { listFleetTokensServerFn } from "#/modules/machines/waiting/fleet-token.functions";
import { listWaitingServersServerFn } from "#/modules/machines/waiting/waiting.functions";

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
