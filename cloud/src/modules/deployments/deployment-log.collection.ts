import { createApiCollection } from "#/collections/query-collection";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { listDeploymentProgressLogsServerFn } from "./deployment.functions";

type EventRow = Awaited<ReturnType<typeof listDeploymentProgressLogsServerFn>>["events"][number];
function createLogs(organizationSlug: string, deploymentId: string, scope: CollectionScope) {
  return createApiCollection<EventRow>({
    queryClient: scope.queryClient,
    queryKey: ["collections", scope.sessionId, scope.userId, organizationSlug, "deployment_logs", deploymentId],
    refetchInterval: false,
    queryFn: async ({ signal }) => {
      const rows: EventRow[] = [];
      let afterSequence: string | null = "0";
      while (afterSequence !== null) {
        const page = await listDeploymentProgressLogsServerFn({ data: { organizationSlug, deploymentId, afterSequence, limit: 50 }, signal });
        rows.push(...page.events);
        afterSequence = page.nextSequence;
      }
      return rows;
    },
    getKey: (row) => row.id,
  });
}
const cache = cachedByCollectionScope(() => new Map<string, ReturnType<typeof createLogs>>());
export function getDeploymentLogsCollection(organizationSlug: string, deploymentId: string, scope: CollectionScope) {
  const collections = cache(organizationSlug, scope);
  let collection = collections.get(deploymentId);
  if (!collection) {
    collection = createLogs(organizationSlug, deploymentId, scope);
    collections.set(deploymentId, collection);
  }
  return collection;
}
