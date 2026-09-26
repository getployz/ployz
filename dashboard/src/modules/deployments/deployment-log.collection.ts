import { collectionOptions } from "@tanstack/react-db";
import { useQuery, type Query, type QueryClient } from "@tanstack/react-query";
import { queryCollectionOptions } from "@tanstack/query-db-collection";
import { cachedByCollectionScope, getDbClient, type CollectionScope } from "#/collections/scope";
import { preloadCollection } from "#/collections/query-collection";
import { listDeploymentProgressLogsServerFn } from "./deployment.functions";

type EventRow = Awaited<ReturnType<typeof listDeploymentProgressLogsServerFn>>["events"][number];
export function createDeploymentLogsCollection(organizationSlug: string, deploymentId: string, scope: CollectionScope,
  readPage: (input: Parameters<typeof listDeploymentProgressLogsServerFn>[0]) => ReturnType<typeof listDeploymentProgressLogsServerFn>,
) {
  const queryKey = ["collections", scope.sessionId, scope.userId, organizationSlug, "deployment_logs", deploymentId];
  const options = {
    queryKey,
    // A finished log never changes, so reopening it reuses the cache; a running log is refetched and polled.
    staleTime: (query: Query<{ events: EventRow[]; finished: boolean }>) => query.state.data?.finished ? Infinity : 0,
    refetchInterval: (query: Query<{ events: EventRow[]; finished: boolean }>) => query.state.data?.finished ? false : 2_000,
    // Each poll resumes from the last held event.
    queryFn: async ({ signal, client }: { signal: AbortSignal; client: QueryClient }) => {
      const rows: EventRow[] = [...client.getQueryData<{ events: EventRow[] }>(queryKey)?.events ?? []];
      let finished = false;
      let afterSequence: string | null | undefined = rows.length ? String(rows.at(-1)?.id) : undefined;
      while (afterSequence !== null) {
        const page = await readPage({ data: { organizationSlug, deploymentId, afterSequence, limit: 50 }, signal });
        finished = page.finished;
        rows.push(...page.events);
        afterSequence = page.nextSequence;
      }
      return { events: rows, finished };
    },
  };
  const collection = getDbClient(scope.queryClient).collection(collectionOptions(queryCollectionOptions<EventRow, typeof options.queryFn, Error>({
    ...options, queryClient: scope.queryClient,
    id: options.queryKey.join(":"), startSync: false,
    select: (page) => page.events, getKey: (row) => row.id,
  })));
  return Object.assign(collection, { queryOptions: options });
}
const cache = cachedByCollectionScope(() => new Map<string, ReturnType<typeof createDeploymentLogsCollection>>());
export function getDeploymentLogsCollection(organizationSlug: string, deploymentId: string, scope: CollectionScope) {
  const collections = cache(organizationSlug, scope);
  let collection = collections.get(deploymentId);
  if (!collection) {
    collection = createDeploymentLogsCollection(organizationSlug, deploymentId, scope, listDeploymentProgressLogsServerFn);
    collections.set(deploymentId, collection);
  }
  return collection;
}

/** Query state of a deployment's log read; the collection keeps rows after a failed refresh. */
export function useDeploymentLogsReadState(collection: ReturnType<typeof getDeploymentLogsCollection>) {
  return useQuery({ ...collection.queryOptions, enabled: false });
}

/** Warm a deployment's Deploy logs when the user reaches for them; the service route's loader warms its Build logs. */
export function preloadDeploymentLogs(organizationSlug: string, deploymentId: string, scope: CollectionScope) {
  void preloadCollection(getDeploymentLogsCollection(organizationSlug, deploymentId, scope)).catch(() => {});
}
