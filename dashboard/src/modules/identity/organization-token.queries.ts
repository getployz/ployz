import { queryOptions } from "@tanstack/react-query";
import { listTokensServerFn } from "#/modules/identity/organization-token.functions";

export const tokensKey = (organizationSlug: string) => ["organization-tokens", organizationSlug] as const;

export function organizationTokensQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: tokensKey(organizationSlug),
    queryFn: () => listTokensServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
  });
}
