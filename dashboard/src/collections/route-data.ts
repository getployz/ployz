import { environmentManager, type FetchInfiniteQueryOptions, type FetchQueryOptions, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { notFound } from "@tanstack/react-router";
import type { CollectionScope } from "./scope";
import { orgStoreOptions } from "./org-store";
import {
  loadWorkspaceEnvironment, organizationStateQueryOptions, preloadWorkspace, readWorkspace,
} from "#/modules/environment-design/workspace.queries";
import type { EnvironmentBySlug } from "#/modules/environment-design/workspace-schemas";
import { activeBuildTailReads } from "#/modules/deployments/deployment.collection";
import type { ConfigQuery, EnvironmentRef } from "@ployz/sdk";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { environmentsQuery, projectsQuery, requireView, storeDeploymentsOptions, storeViewOptions } from "#/modules/config-store/store-view.queries";

/**
 * Route loaders call only the helpers in this file.
 * - `require*` awaits a route decision (access, not-found, redirect) on server and client.
 * - `prefetch*` awaits on the server so SSR HTML is complete, and never blocks client navigation.
 */
type RouteDataContext = {
  queryClient: QueryClient;
  session: { session: { id: string }; user: { id: string } };
};

function scopeOf(context: RouteDataContext): CollectionScope {
  return { queryClient: context.queryClient, sessionId: context.session.session.id, userId: context.session.user.id };
}

export async function requireOrganization(context: RouteDataContext, organizationSlug: string) {
  const { activeOrganization } = await context.queryClient.ensureQueryData(organizationStateQueryOptions(organizationSlug));
  if (activeOrganization?.slug !== organizationSlug) throw notFound();
  return activeOrganization;
}

/** Billing exists only on Ployz-hosted Cloud. */
export async function requireBilling(context: RouteDataContext, organizationSlug: string) {
  const organization = await context.queryClient.ensureQueryData(organizationStateQueryOptions(organizationSlug));
  if (!organization.billingEnabled) throw notFound();
}

export async function requireWorkspace(context: RouteDataContext, organizationSlug: string) {
  return readWorkspace(await preloadWorkspace(organizationSlug, scopeOf(context)));
}

export async function requireEnvironment(context: RouteDataContext, input: EnvironmentBySlug) {
  return loadWorkspaceEnvironment(input, scopeOf(context));
}

/** The Organization's Projects in the Config Store. */
export async function requireStoreProjects(context: RouteDataContext, organizationSlug: string) {
  return requireView(await context.queryClient.ensureQueryData(storeViewOptions(organizationSlug, scopeOf(context), projectsQuery()))).projects;
}

/** The Config Store Environment a route names by its Project's name and its own; not found otherwise. */
export async function requireStoreEnvironment(context: RouteDataContext, input: EnvironmentBySlug) {
  const result = await context.queryClient.ensureQueryData(
    storeViewOptions(input.organizationSlug, scopeOf(context), environmentsQuery(input.projectSlug)));
  const environment = result.ok ? result.value.environments.find((row) => row.name === input.environmentSlug) : undefined;
  if (!environment) throw notFound();
  return environment;
}

/** SSR failure fails the organization route: no org page can render without the Org Store. */
export async function prefetchOrgStore(context: RouteDataContext, organizationSlug: string) {
  const ready = context.queryClient.ensureQueryData(orgStoreOptions(organizationSlug, scopeOf(context)));
  // The shell's content gate owns client pending and error state.
  if (environmentManager.isServer()) await ready;
  else void ready.catch(() => {});
}

/**
 * Starts every read together. Never throws: an SSR failure is retried by the page's
 * `useSuspenseQuery`, whose boundary owns the error.
 */
// ponytail: typed by key only; reads return different data, and a prefetch returns none.
export async function prefetchRemote(context: RouteDataContext, ...reads: Array<Pick<FetchQueryOptions, "queryKey">>) {
  const ready = Promise.all(reads.map((options) => context.queryClient.prefetchQuery(options)));
  if (environmentManager.isServer()) await ready;
}

/**
 * The build tails of the Environment's active attempts that build images, which the canvas's bottom bar and list read.
 * Finding them waits for the Org Store, so the client starts it in the background, like `prefetchOrgStore`.
 */
export async function prefetchActiveBuildTails(context: RouteDataContext, input: EnvironmentBySlug) {
  const ready = requireEnvironment(context, input)
    .then((environment) => activeBuildTailReads(input.organizationSlug, environment.id, scopeOf(context)))
    .then((reads) => prefetchRemote(context, ...reads));
  if (environmentManager.isServer()) await ready;
  else void ready.catch(() => {});
}

/** `prefetchRemote` for Config Store views, started together. Nothing while the Store is dark. */
export async function prefetchStoreViews(context: RouteDataContext, organizationSlug: string, ...queries: ConfigQuery[]) {
  if (!storeEnabled) return;
  await prefetchRemote(context, ...queries.map((query) => storeViewOptions(organizationSlug, scopeOf(context), query)));
}

/**
 * An Environment page's Store reads, started together: `queries`, and the first page of its Deployments (the bottom
 * bar's in-flight one, the Deployments list). Nothing while the Store is dark.
 */
export async function prefetchStoreEnvironment(context: RouteDataContext, organizationSlug: string, environment: EnvironmentRef,
  ...queries: ConfigQuery[]) {
  if (!storeEnabled) return;
  await Promise.all([
    prefetchStoreViews(context, organizationSlug, ...queries),
    prefetchRemotePages(context, storeDeploymentsOptions(organizationSlug, scopeOf(context), environment)),
  ]);
}

/** `prefetchRemote` for a paged read: its first page. */
export async function prefetchRemotePages<T, K extends QueryKey, P>(context: RouteDataContext, options: FetchInfiniteQueryOptions<T, Error, T, K, P>) {
  const ready = context.queryClient.prefetchInfiniteQuery(options);
  if (environmentManager.isServer()) await ready;
}

/**
 * Prefetches a loader decides from the Org Store, started together (`false` skips one). During SSR it first awaits the Org
 * Store, whose failure fails the route like `prefetchOrgStore`. On the client it never waits: the gate owns the Org Store's
 * pending and retryable error state, and `start` decides from the rows already in memory.
 */
export async function prefetchFromOrgStore(context: RouteDataContext, organizationSlug: string,
  start: (scope: CollectionScope) => Array<Promise<void> | false | null>) {
  const scope = scopeOf(context);
  const ready = context.queryClient.ensureQueryData(orgStoreOptions(organizationSlug, scope));
  if (environmentManager.isServer()) await ready;
  else void ready.catch(() => {});
  await Promise.all(start(scope));
}
