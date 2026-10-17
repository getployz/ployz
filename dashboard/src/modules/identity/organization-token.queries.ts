import { queryOptions } from "@tanstack/react-query";
import { listTokensServerFn } from "#/modules/identity/organization-token.functions";

export const tokensKey = (organizationSlug: string) => ["organization-tokens", organizationSlug] as const;

/** The tokens the signed-in person can revoke: their own, or every one in the Organization for owners and admins. */
export function organizationTokensQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: tokensKey(organizationSlug),
    queryFn: () => listTokensServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
  });
}
