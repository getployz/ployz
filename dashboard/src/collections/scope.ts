import type { QueryClient } from "@tanstack/react-query";
import { DbClient, type DehydrateDbClientOptions } from "@tanstack/react-db";

/**
 * A collection's SSR copy is its Query, which carries its rows, its readiness and a change collection's cursor; DB
 * hydration restores rows without readiness. DB dehydrates only live queries, whose snapshots hold the page until each
 * collection syncs from its Query.
 */
class QueryBackedDbClient extends DbClient {
  override dehydrate(options?: DehydrateDbClientOptions) {
    return super.dehydrate({ shouldDehydrateCollection: () => false, ...options });
  }
}

const dbClients = new WeakMap<QueryClient, DbClient>();

/** DB and Query share the router's request/browser lifetime. */
export function getDbClient(queryClient: QueryClient) {
  let client = dbClients.get(queryClient);
  if (!client) {
    client = new QueryBackedDbClient();
    dbClients.set(queryClient, client);
  }
  return client;
}

/** Org Store scope: collections are org-wide. Filter environments in live queries, never in keys. */
export type CollectionScope = { queryClient: QueryClient; sessionId: string; userId: string };

/** The scope a signed-in session's user reads the Org Store in. */
export function scopeOf(queryClient: QueryClient, session: { session: { id: string }; user: { id: string } }): CollectionScope {
  return { queryClient, sessionId: session.session.id, userId: session.user.id };
}

/** The identity of one Organization's data in one session: what a scope's caches are keyed by. */
export const scopeKey = (scope: CollectionScope, organizationSlug: string) => JSON.stringify([scope.sessionId, scope.userId, organizationSlug]);

/** The Query key of an Org Store collection. Keys hold session, user and Organization only. */
export const collectionKey = (scope: CollectionScope, organizationSlug: string, table: string) =>
  ["collections", scope.sessionId, scope.userId, organizationSlug, table] as const;

/** The Query key of an Organization's Org Store readiness. */
export const orgStoreKey = (scope: CollectionScope, organizationSlug: string) =>
  ["org-store", scope.sessionId, scope.userId, organizationSlug] as const;

export function cachedByCollectionScope<T>(create: (organizationSlug: string, scope: CollectionScope) => T) {
  const clients = new WeakMap<QueryClient, Map<string, T>>();
  return (organizationSlug: string, scope: CollectionScope): T => {
    let cache = clients.get(scope.queryClient);
    if (!cache) {
      cache = new Map();
      clients.set(scope.queryClient, cache);
    }
    const key = scopeKey(scope, organizationSlug);
    const existing = cache.get(key);
    if (existing) return existing;
    const collection = create(organizationSlug, scope);
    cache.set(key, collection);
    return collection;
  };
}
