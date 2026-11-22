import { environmentManager, type FetchInfiniteQueryOptions, type FetchQueryOptions, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { scopeOf as scopeOfSession, type CollectionScope } from "./scope";
import { orgStoreOptions } from "./org-store";

/**
 * Route loaders call only `require*` and `prefetch*` helpers from `*.route-data.ts` files, and start reads with `stream*`.
 * This file holds the generic primitives; each feature keeps the helpers that name its own reads next to them.
 * - `require*` awaits a route decision (access, not-found, redirect) on server and client.
 * - `prefetch*` awaits on the server so SSR HTML is complete, and never blocks client navigation.
 * - `stream*` never awaits: the page renders its fallback and the read streams in after the HTML.
 */
export type RouteDataContext = {
  queryClient: QueryClient;
  session: { session: { id: string }; user: { id: string } };
};

export function scopeOf(context: RouteDataContext): CollectionScope {
  return scopeOfSession(context.queryClient, context.session);
}

/** SSR failure fails the organization route: no org page can render without the Org Store. */
export async function prefetchOrgStore(context: RouteDataContext, organizationSlug: string) {
  const ready = context.queryClient.ensureQueryData(orgStoreOptions(organizationSlug, scopeOf(context)));
  // The shell's content gate owns client pending and error state.
  if (environmentManager.isServer()) await ready;
  // The gate reads the failed collection and shows the retry; an unhandled rejection here would only add noise.
  else void ready.catch(() => {});
}

/**
 * Starts every read together. Never throws: an SSR failure is retried by the page's
 * `useSuspenseQuery`, whose boundary owns the error.
 */
// ponytail: typed by key only; reads return different data, and a prefetch returns none.
export async function prefetchRemote(context: Pick<RouteDataContext, "queryClient">, ...reads: Array<Pick<FetchQueryOptions, "queryKey">>) {
  const ready = Promise.all(reads.map((options) => context.queryClient.prefetchQuery(options)));
  if (environmentManager.isServer()) await ready;
}

/**
 * Starts every read and returns at once, on the server too: SSR sends the page's skeleton and streams the data after
 * it. For reads a page can show late but that must never hold its HTML back.
 */
export function streamRemote(context: RouteDataContext, ...reads: Array<Pick<FetchQueryOptions, "queryKey">>) {
  for (const options of reads) void context.queryClient.prefetchQuery(options);
}

/** `prefetchRemote` for a paged read: its first page. */
export async function prefetchRemotePages<T, K extends QueryKey, P>(context: RouteDataContext, options: FetchInfiniteQueryOptions<T, Error, T, K, P>) {
  const ready = context.queryClient.prefetchInfiniteQuery(options);
  if (environmentManager.isServer()) await ready;
}
