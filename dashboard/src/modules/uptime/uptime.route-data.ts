import type { FetchQueryOptions } from "@tanstack/react-query";
import { streamRemote, type RouteDataContext } from "#/collections/route-data";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";

/**
 * `streamRemote` for uptime reads, on a Cloud that runs the uptime checker only: starts them once the Organization
 * state says so, and returns at once. A failed state read starts nothing.
 */
export function streamRemoteUptime(context: RouteDataContext, organizationSlug: string, ...reads: Array<Pick<FetchQueryOptions, "queryKey">>) {
  void context.queryClient.ensureQueryData(organizationStateQueryOptions(organizationSlug))
    .then(({ uptimeEnabled }) => { if (uptimeEnabled) streamRemote(context, ...reads); }, () => {});
}
