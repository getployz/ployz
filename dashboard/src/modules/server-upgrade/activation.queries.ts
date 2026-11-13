import { queryOptions, useQuery } from "@tanstack/react-query";
import { readActivationServerFn } from "./activation.functions";

export const activationKeys = { all: ["activation"] as const };

/** Each Server's own account of its activation. A pending one is asked again until it settles. */
export function activationQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...activationKeys.all, organizationSlug] as const,
    queryFn: () => readActivationServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
    refetchInterval: (query) => query.state.data?.some((server) => server.activation?.state === "activating") ? 5_000 : false,
  });
}

export function useActivation(organizationSlug: string, enabled = true) {
  return useQuery({ ...activationQueryOptions(organizationSlug), enabled });
}
