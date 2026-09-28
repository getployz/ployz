import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { listMissingPrEnvironmentGrantsServerFn } from "./plan-functions";

/** The organization's GitHub App installations that haven't accepted what PR Environments need. */
export function missingPrEnvironmentGrantsQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["pr-environments", "missing-grants", organizationSlug] as const,
    queryFn: () => listMissingPrEnvironmentGrantsServerFn({ data: { organizationSlug } }),
    // Permissions change in GitHub, when an owner approves them there.
    staleTime: 60_000,
  });
}

/** Where to approve the installation behind `installationId`, when it hasn't accepted what PR Environments need; else null. */
export function useMissingPrEnvironmentGrant(organizationSlug: string, installationId: number) {
  const { data } = useSuspenseQuery(missingPrEnvironmentGrantsQueryOptions(organizationSlug));
  return data.find((missing) => missing.installationId === installationId)?.url ?? null;
}
