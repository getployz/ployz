import { queryOptions, useQuery } from "@tanstack/react-query";
import { listStrayNamespacesServerFn } from "./namespace-cleanup.functions";

/**
 * Which of `namespaces`, seen on the Servers now, no Environment owns. Keyed by what the Servers report, so a Namespace
 * that just appeared (a new Environment's first Deploy) is checked afresh, never read against an older answer.
 */
export function strayNamespacesQueryOptions(organizationSlug: string, namespaces: readonly string[]) {
  const seen = [...new Set(namespaces)].sort();
  return queryOptions({
    queryKey: ["servers", "stray-namespaces", organizationSlug, seen] as const,
    queryFn: () => listStrayNamespacesServerFn({ data: { organizationSlug, namespaces: seen } }),
    // An Environment deleted elsewhere leaves its Namespace owned until a minute's refetch: the offer only comes later.
    staleTime: 60_000,
    enabled: seen.length > 0,
  });
}

/** The Namespaces among `namespaces` no Environment owns; none until Cloud has answered. */
export function useStrayNamespaces(organizationSlug: string, namespaces: readonly string[]) {
  return new Set(useQuery(strayNamespacesQueryOptions(organizationSlug, namespaces)).data ?? []);
}
