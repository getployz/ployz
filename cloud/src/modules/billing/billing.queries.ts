import { queryOptions } from "@tanstack/react-query";
import { getBillingStateServerFn } from "#/modules/billing/billing.functions";

export const billingKeys = {
  all: ["billing"] as const,
  org: (organizationSlug: string) =>
    [...billingKeys.all, organizationSlug] as const,
  state: (organizationSlug: string) =>
    [...billingKeys.org(organizationSlug), "state"] as const,
};

export function billingStateQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: billingKeys.state(organizationSlug),
    queryFn: ({ signal }) =>
      getBillingStateServerFn({
        data: { organizationSlug },
        signal,
      }),
    staleTime: 60_000,
  });
}
