import type { QueryClient } from "@tanstack/react-query";
import { DbClient } from "@tanstack/react-db";

const dbClients = new WeakMap<QueryClient, DbClient>();

/** DB and Query share the router's request/browser lifetime. */
export function getDbClient(queryClient: QueryClient) {
  let client = dbClients.get(queryClient);
  if (!client) {
    client = new DbClient();
    dbClients.set(queryClient, client);
  }
  return client;
}

export type CollectionScope = { queryClient: QueryClient; sessionId: string; userId: string; environmentSlug?: string };

export function cachedByCollectionScope<T>(create: (organizationSlug: string, scope: CollectionScope) => T) {
  const clients = new WeakMap<QueryClient, Map<string, T>>();
  return (organizationSlug: string, scope: CollectionScope): T => {
    let cache = clients.get(scope.queryClient);
    if (!cache) {
      cache = new Map();
      clients.set(scope.queryClient, cache);
    }
    const key = JSON.stringify([scope.sessionId, scope.userId, organizationSlug, scope.environmentSlug]);
    const existing = cache.get(key);
    if (existing) return existing;
    const collection = create(organizationSlug, scope);
    cache.set(key, collection);
    return collection;
  };
}
