import { environmentManager, type FetchQueryOptions } from "@tanstack/react-query";
import { notFound } from "@tanstack/react-router";
import type { EnvironmentRef } from "@ployz/sdk";
import { prefetchRemote, prefetchRemotePages, scopeOf, streamRemote, type RouteDataContext } from "#/collections/route-data";
import type { AwaitableQuery, ClusterQuery } from "./cluster-queries";
import { configQuery, configsQuery, environmentsQuery, projectsQuery, requireView, storeDeploymentsOptions, storeViewOptions } from "./store-view.queries";

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

/** `prefetchStoreViews` and the page's other Remote Reads, all started together. */
export async function prefetchRemoteWithStoreViews(context: RouteDataContext, organizationSlug: string, queries: AwaitableQuery[],
  ...reads: Array<Pick<FetchQueryOptions, "queryKey">>) {
  await prefetchRemote(context, ...queries.map((query) => storeViewOptions(organizationSlug, scopeOf(context), query)), ...reads);
}

/** `prefetchRemote` for Config Store views, started together. */
export async function prefetchStoreViews(context: RouteDataContext, organizationSlug: string, ...queries: AwaitableQuery[]) {
  await prefetchRemote(context, ...queries.map((query) => storeViewOptions(organizationSlug, scopeOf(context), query)));
}

/** `streamRemote` for Config Store views that wait on the Cluster: the only way a loader starts one. */
export function streamStoreViews(context: RouteDataContext, organizationSlug: string, ...queries: ClusterQuery[]) {
  streamRemote(context, ...queries.map((query) => storeViewOptions(organizationSlug, scopeOf(context), query)));
}

/**
 * An Environment page's reads, started together: Store `queries`, the first page of its Deployments (the canvas's
 * in-flight ones, the Deployments list), and its other Remote Reads.
 */
export async function prefetchStoreEnvironment(context: RouteDataContext, organizationSlug: string, environment: EnvironmentRef,
  queries: AwaitableQuery[], ...reads: Array<Pick<FetchQueryOptions, "queryKey">>) {
  await Promise.all([
    prefetchRemoteWithStoreViews(context, organizationSlug, queries, ...reads),
    prefetchRemotePages(context, storeDeploymentsOptions(organizationSlug, scopeOf(context), environment)),
  ]);
}

/** The files of the Config `resourceId` names, if it names one: the one read a Config's drawer adds to its Environment's. */
export async function prefetchStoreConfig(context: RouteDataContext, organizationSlug: string, environment: EnvironmentRef, resourceId: string) {
  const scope = scopeOf(context);
  const ready = context.queryClient.ensureQueryData(storeViewOptions(organizationSlug, scope, configsQuery(environment))).then((result) => {
    const config = result.ok ? result.value.configs.find((candidate) => candidate.id === resourceId) : undefined;
    return config && context.queryClient.prefetchQuery(storeViewOptions(organizationSlug, scope, configQuery(environment, `@${config.id}`)));
  // The drawer fetches the same reads itself when this prefetch fails.
  }).catch(() => {});
  if (environmentManager.isServer()) await ready;
}
