import { queryOptions } from "@tanstack/react-query";
import {
  getBillingStateServerFn,
  getCustomDomainsAllowedServerFn,
} from "#/modules/billing/billing.functions";

export const billingKeys = {
  all: ["billing"] as const,
  org: (organizationSlug: string) =>
    [...billingKeys.all, organizationSlug] as const,
  state: (organizationSlug: string) =>
    [...billingKeys.org(organizationSlug), "state"] as const,
  customDomainCapability: (organizationSlug: string) =>
    [...billingKeys.org(organizationSlug), "customDomainCapability"] as const,
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

export function customDomainCapabilityQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: billingKeys.customDomainCapability(organizationSlug),
    queryFn: ({ signal }) =>
      getCustomDomainsAllowedServerFn({
        data: { organizationSlug },
        signal,
      }),
    staleTime: 60_000,
  });
}
