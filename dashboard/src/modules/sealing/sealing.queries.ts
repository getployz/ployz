import { queryOptions } from "@tanstack/react-query";
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
