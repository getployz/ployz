import { infiniteQueryOptions, type InfiniteData, queryOptions, useSuspenseInfiniteQuery, useSuspenseQuery } from "@tanstack/react-query";
import { parseServiceConfig, type ServiceConfig } from "@ployz/sdk/config";
import type { CollectionScope } from "#/collections/scope";
import { getDeploymentAttemptServerFn, listEnvironmentDeploymentsServerFn, listNodeDeploymentsServerFn } from "./deployment.functions";

export type DeploymentHistoryRow = Awaited<ReturnType<typeof listEnvironmentDeploymentsServerFn>>["items"][number];
export type NodeDeployment = NonNullable<Awaited<ReturnType<typeof listNodeDeploymentsServerFn>>["running"]>;

const historyKey = (organizationSlug: string): Array<string | null> => ["deployment-history", organizationSlug];

/** Every deployment list pages the same way: newest first, each page naming the attempt the next one starts before. */
function pagedHistoryOptions<Page extends { next: string | null }>(
  queryKey: Array<string | null>, readPage: (before: string | undefined, signal: AbortSignal) => Promise<Page>,
) {
  return infiniteQueryOptions<Page, Error, InfiniteData<Page, string | undefined>, Array<string | null>, string | undefined>({
    queryKey,
    // Refetched when the change stream names a deployment row change (invalidateDeploymentHistory).
    staleTime: Infinity,
    initialPageParam: undefined,
    getNextPageParam: (page) => page.next ?? undefined,
    queryFn: ({ pageParam, signal }) => readPage(pageParam, signal),
  });
}

/** An environment's attempts, a page at a time. */
export function environmentDeploymentsQueryOptions(organizationSlug: string, environmentId: string) {
  return pagedHistoryOptions([...historyKey(organizationSlug), "environment", environmentId], (before, signal) =>
    listEnvironmentDeploymentsServerFn({ data: { organizationSlug, environmentId, before }, signal }));
}

/** A node's History a page at a time; the first page also names its Running attempt. */
export function nodeDeploymentsQueryOptions(organizationSlug: string, environmentId: string, nodeId: string) {
  return pagedHistoryOptions([...historyKey(organizationSlug), "node", environmentId, nodeId], (before, signal) =>
    listNodeDeploymentsServerFn({ data: { organizationSlug, environmentId, nodeId, before }, signal }));
}

export function useNodeDeployments(organizationSlug: string, environmentId: string, nodeId: string) {
  return useSuspenseInfiniteQuery(nodeDeploymentsQueryOptions(organizationSlug, environmentId, nodeId));
}

/** One attempt's row and the service configs it deployed; null without an id, or when the organization has no such attempt. */
export function deploymentAttemptQueryOptions(organizationSlug: string, deploymentId: string | null) {
  return queryOptions({
    queryKey: [...historyKey(organizationSlug), "attempt", deploymentId],
    // A queued attempt's snapshots are rewritten with its row, which the change stream names (invalidateDeploymentHistory).
    staleTime: Infinity,
    initialData: () => deploymentId === null ? null : undefined,
    queryFn: ({ signal }) => deploymentId === null ? null : getDeploymentAttemptServerFn({ data: { organizationSlug, deploymentId }, signal }),
  });
}

/** A deployment row changed: its attempt read and every page that may hold it refetch. */
export function invalidateDeploymentHistory(organizationSlug: string, scope: CollectionScope) {
  void scope.queryClient.invalidateQueries({ queryKey: historyKey(organizationSlug) });
}

/**
 * The service configs one attempt deployed, by node id, for the service panel's Details. Suspends until they arrive.
 * A removed service has none: the attempt holds no snapshot of it.
 */
export function useAttemptServiceConfigs(organizationSlug: string, deploymentId: string): Map<string, ServiceConfig> {
  const { data } = useSuspenseQuery(deploymentAttemptQueryOptions(organizationSlug, deploymentId));
  return new Map(data?.serviceConfigs.map((row) => [row.nodeId, parseServiceConfig(row.config)]));
}
