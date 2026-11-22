/** @data-source remote: a claim link's Organization and state, prefetched by /claim and fresh for a minute; keeping the Organization leaves the page */
import { queryOptions } from "@tanstack/react-query";
import { readClaimServerFn } from "#/modules/sandbox/claim.functions";

export type Claim = Awaited<ReturnType<typeof readClaimServerFn>>;

export function claimQueryOptions(secret: string) {
  return queryOptions({
    queryKey: ["claim", secret] as const,
    queryFn: () => readClaimServerFn({ data: { secret } }),
    staleTime: 60_000,
  });
}
