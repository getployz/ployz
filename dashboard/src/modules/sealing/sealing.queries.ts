import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { isUnauthorized } from "#/lib/error-message";
import { readSealingFn } from "./sealing.functions";

/** What the Sealed controls show. A seal never trusts it: `useSealing`'s `prepare` reads afresh. */
export function sealingQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["sealing", organizationSlug] as const,
    queryFn: () => readSealingFn({ data: { organizationSlug } }),
    // A first Server pairs rarely and from elsewhere.
    staleTime: 60_000,
  });
}

/** One fresh read of the Organization's sealing, never the cache. A failure other than a sign-out becomes `unread()`. */
export function freshSealing(queryClient: QueryClient, organizationSlug: string, unread: () => Error) {
  return queryClient.fetchQuery({ ...sealingQueryOptions(organizationSlug), staleTime: 0 })
    .catch((error) => { throw isUnauthorized(error) ? error : unread(); });
}
