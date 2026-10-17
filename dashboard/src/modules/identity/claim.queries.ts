import { queryOptions } from "@tanstack/react-query";
import { readClaimServerFn } from "#/modules/identity/claim.functions";

export type Claim = Awaited<ReturnType<typeof readClaimServerFn>>;

export function claimQueryOptions(secret: string) {
  return queryOptions({
    queryKey: ["claim", secret] as const,
    queryFn: () => readClaimServerFn({ data: { secret } }),
    staleTime: 60_000,
  });
}
