import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { listMissingPrEnvironmentGrantsServerFn, listMissingStorePrGrantsServerFn } from "./plan-functions";

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

/** Over the Config Store: the installations a Project's PR plans deploy through that haven't accepted what they need. */
export function missingStorePrGrantsQueryOptions(organizationSlug: string, projectSlug: string) {
  return queryOptions({
    queryKey: ["pr-environments", "missing-store-grants", organizationSlug, projectSlug] as const,
    queryFn: () => listMissingStorePrGrantsServerFn({ data: { organizationSlug, projectSlug } }),
    // Permissions change in GitHub, when an owner approves them there.
    staleTime: 60_000,
  });
}

/** Where to approve `installationId` for the Project's PR Environments, when it hasn't accepted what they need; else null. */
export function useMissingStorePrGrant(organizationSlug: string, projectSlug: string, installationId: number) {
  const { data } = useSuspenseQuery(missingStorePrGrantsQueryOptions(organizationSlug, projectSlug));
  return data.find((missing) => missing.installationId === installationId)?.url ?? null;
}
