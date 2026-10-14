import { queryOptions } from "@tanstack/react-query";
import { listGithubOfferServerFn, listOrganizationGithubServerFn } from "#/modules/github/github-connect.functions";
import { githubKeys } from "#/modules/github/github.queries";

export function organizationGithubQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: githubKeys.accounts(organizationSlug),
    queryFn: () => listOrganizationGithubServerFn({ data: { organizationSlug } }),
  });
}

export function githubOfferQueryOptions(organizationSlug: string, offer: string) {
  return queryOptions({
    queryKey: githubKeys.offer(organizationSlug, offer),
    queryFn: () => listGithubOfferServerFn({ data: { organizationSlug, offer } }),
    staleTime: Infinity,
    retry: false,
  });
}
