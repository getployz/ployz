import type { QueryClient } from "@tanstack/react-query";
import { createApiCollection } from "#/collections/query-collection";
import type { CollectionScope } from "#/collections/scope";
import {
  disconnectAgentServerFn,
  listAccountSessionsServerFn,
  listConnectedAgentsServerFn,
  listLinkedAccountsServerFn,
  revokeAccountSessionServerFn,
} from "#/modules/account/account.functions";

export type AccountSessionRow = Awaited<ReturnType<typeof listAccountSessionsServerFn>>[number];
export type LinkedAccountRow = Awaited<ReturnType<typeof listLinkedAccountsServerFn>>[number];
export type ConnectedAgentRow = Awaited<ReturnType<typeof listConnectedAgentsServerFn>>[number];

const STALE_MS = 60_000;

/** The signed-in user's live sessions, CLI devices included. Only this user ends them, so each revoke lands at once. */
export function accountSessionsRead(scope: CollectionScope) {
  return {
    queryKey: ["collections", scope.sessionId, scope.userId, "account_sessions"],
    queryFn: ({ signal }: { signal: AbortSignal }) => listAccountSessionsServerFn({ signal }),
    staleTime: STALE_MS,
  };
}

/** The signed-in user's sign-in methods. */
export function linkedAccountsRead(scope: CollectionScope) {
  return {
    queryKey: ["collections", scope.sessionId, scope.userId, "linked_accounts"],
    queryFn: ({ signal }: { signal: AbortSignal }) => listLinkedAccountsServerFn({ signal }),
    staleTime: STALE_MS,
  };
}

/** The agents the signed-in user connected on /connect that can still act. */
export function connectedAgentsRead(scope: CollectionScope) {
  return {
    queryKey: ["collections", scope.sessionId, scope.userId, "connected_agents"],
    queryFn: ({ signal }: { signal: AbortSignal }) => listConnectedAgentsServerFn({ signal }),
    staleTime: STALE_MS,
  };
}

function scoped<C>(create: (scope: CollectionScope) => C) {
  const caches = new WeakMap<QueryClient, Map<string, C>>();
  return (scope: CollectionScope) => {
    let cache = caches.get(scope.queryClient);
    if (!cache) caches.set(scope.queryClient, (cache = new Map()));
    const key = `${scope.sessionId}:${scope.userId}`;
    let collection = cache.get(key);
    if (!collection) cache.set(key, (collection = create(scope)));
    return collection;
  };
}

export const getAccountSessionsCollection = scoped((scope) =>
  createApiCollection({ queryClient: scope.queryClient, getKey: (row: AccountSessionRow) => row.id, ...accountSessionsRead(scope) }));

export const getLinkedAccountsCollection = scoped((scope) =>
  createApiCollection({ queryClient: scope.queryClient, getKey: (row: LinkedAccountRow) => row.id, ...linkedAccountsRead(scope) }));

export const getConnectedAgentsCollection = scoped((scope) =>
  createApiCollection({ queryClient: scope.queryClient, getKey: (row: ConnectedAgentRow) => row.id, ...connectedAgentsRead(scope) }));

/** Ends a browser session: the row leaves at once, and the refetch after brings it back if the server refused. */
export function revokeAccountSession(scope: CollectionScope, sessionId: string) {
  const sessions = getAccountSessionsCollection(scope);
  sessions.utils.writeDelete(sessionId);
  return revokeAccountSessionServerFn({ data: { sessionId } }).finally(() => void sessions.utils.refetch());
}

/** Disconnects an agent: the row leaves at once, and the refetch after brings it back if the server refused. */
export function disconnectAgent(scope: CollectionScope, consentId: string) {
  const agents = getConnectedAgentsCollection(scope);
  agents.utils.writeDelete(consentId);
  return disconnectAgentServerFn({ data: { consentId } }).finally(() => void agents.utils.refetch());
}
