import { environmentManager, type FetchInfiniteQueryOptions, type FetchQueryOptions, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { notFound } from "@tanstack/react-router";
import type { CollectionScope } from "./scope";
import { orgStoreOptions } from "./org-store";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import type { ConfigQuery, EnvironmentRef } from "@ployz/sdk";
import { configQuery, configsQuery, environmentsQuery, projectsQuery, requireView, storeDeploymentsOptions, storeViewOptions } from "#/modules/config-store/store-view.queries";

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

/** The Organization's Projects in the Config Store. */
export async function requireStoreProjects(context: RouteDataContext, organizationSlug: string) {
  return requireView(await context.queryClient.ensureQueryData(storeViewOptions(organizationSlug, scopeOf(context), projectsQuery()))).projects;
}

/** The Config Store Environment a route names by its Project's name and its own; not found otherwise. */
export async function requireStoreEnvironment(context: RouteDataContext,
  input: { organizationSlug: string; projectSlug: string; environmentSlug: string }) {
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

/** `prefetchStoreViews` and the page's other Remote Reads, all started together. */
export async function prefetchRemoteWithStoreViews(context: RouteDataContext, organizationSlug: string, queries: ConfigQuery[],
  ...reads: Array<Pick<FetchQueryOptions, "queryKey">>) {
  await prefetchRemote(context, ...queries.map((query) => storeViewOptions(organizationSlug, scopeOf(context), query)), ...reads);
}

/** `prefetchRemote` for Config Store views, started together. */
export async function prefetchStoreViews(context: RouteDataContext, organizationSlug: string, ...queries: ConfigQuery[]) {
  await prefetchRemote(context, ...queries.map((query) => storeViewOptions(organizationSlug, scopeOf(context), query)));
}

/**
 * An Environment page's Store reads, started together: `queries`, and the first page of its Deployments (the bottom
 * bar's in-flight one, the Deployments list).
 */
export async function prefetchStoreEnvironment(context: RouteDataContext, organizationSlug: string, environment: EnvironmentRef,
  ...queries: ConfigQuery[]) {
  await Promise.all([
    prefetchStoreViews(context, organizationSlug, ...queries),
    prefetchRemotePages(context, storeDeploymentsOptions(organizationSlug, scopeOf(context), environment)),
  ]);
}

/** The files of the Config `resourceId` names, if it names one: the one read a Config's drawer adds to its Environment's. */
export async function prefetchStoreConfig(context: RouteDataContext, organizationSlug: string, environment: EnvironmentRef, resourceId: string) {
  const scope = scopeOf(context);
  const ready = context.queryClient.ensureQueryData(storeViewOptions(organizationSlug, scope, configsQuery(environment))).then((result) => {
    const config = result.ok ? result.value.configs.find((candidate) => candidate.id === resourceId) : undefined;
    return config && context.queryClient.prefetchQuery(storeViewOptions(organizationSlug, scope, configQuery(environment, `@${config.id}`)));
  }).catch(() => {});
  if (environmentManager.isServer()) await ready;
}

/** `prefetchRemote` for a paged read: its first page. */
export async function prefetchRemotePages<T, K extends QueryKey, P>(context: RouteDataContext, options: FetchInfiniteQueryOptions<T, Error, T, K, P>) {
  const ready = context.queryClient.prefetchInfiniteQuery(options);
  if (environmentManager.isServer()) await ready;
}
