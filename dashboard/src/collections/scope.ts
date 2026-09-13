import type { QueryClient } from "@tanstack/react-query";

export type CollectionScope = { queryClient: QueryClient; sessionId: string; userId: string };

export function cachedByCollectionScope<T>(create: (organizationSlug: string, scope: CollectionScope) => T) {
  const clients = new WeakMap<QueryClient, Map<string, T>>();
  return (organizationSlug: string, scope: CollectionScope): T => {
    let cache = clients.get(scope.queryClient);
    if (!cache) {
      cache = new Map();
      clients.set(scope.queryClient, cache);
    }
    const key = JSON.stringify([scope.sessionId, scope.userId, organizationSlug]);
    const existing = cache.get(key);
    if (existing) return existing;
    const collection = create(organizationSlug, scope);
    cache.set(key, collection);
    return collection;
  };
}
