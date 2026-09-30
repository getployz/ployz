import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { listMissingStorePrGrantsServerFn } from "./store-pr-grants.functions";

/** The installations a Project's PR plans deploy through that haven't accepted what they need. */
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
