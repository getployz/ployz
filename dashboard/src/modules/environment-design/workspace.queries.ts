import { toast } from "sonner";
import { environmentManager, queryOptions, type QueryClient } from "@tanstack/react-query";
import { useLiveQuery } from "@tanstack/react-db";
import { useSyncExternalStore } from "react";
import { notFound } from "@tanstack/react-router";
import { getProjectsCollection, getEnvironmentSummariesCollection, type EnvironmentSummary } from "#/collections/collections";
import { preloadCollection } from "#/collections/query-collection";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { EnvironmentBySlug } from "./workspace-schemas";
import { getOrganizationStateServerFn, syncOrganizationSlugServerFn } from "./workspace-functions";

export const organizationKeys = {
  all: ["organization"] as const,
  state: (organizationSlug?: string | null) =>
    [...organizationKeys.all, organizationSlug ?? null, "state"] as const,
};

export function organizationStateQueryOptions(organizationSlug?: string | null) {
  return queryOptions({
    queryKey: organizationKeys.state(organizationSlug),
    // Membership and the active organization are authoritative on every mount.
    staleTime: 0,
    queryFn: ({ signal }) =>
      getOrganizationStateServerFn({
        data: organizationSlug === undefined || organizationSlug === null
          ? {}
          : { organizationSlug },
        signal,
      }),
  });
}


export function workspaceCollections(organizationSlug: string, scope: CollectionScope) {
  return {
    projects: getProjectsCollection(organizationSlug, scope),
    environments: getEnvironmentSummariesCollection(organizationSlug, scope),
  };
}

export async function preloadWorkspace(organizationSlug: string, scope: CollectionScope) {
  const collections = workspaceCollections(organizationSlug, scope);
  await Promise.all(Object.values(collections).map(preloadCollection));
  return collections;
}

/** The Environment a project opens: its Default Environment, or its oldest when that is unset. */
export function resolveDefaultEnvironment<E extends Pick<EnvironmentSummary, "id" | "projectId" | "createdAt">>(
  project: { id: string; defaultEnvironmentId: string | null },
  environments: Iterable<E>,
) {
  const own = [...environments].filter((environment) => environment.projectId === project.id);
  return own.find((environment) => environment.id === project.defaultEnvironmentId)
    ?? own.sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime())[0]
    ?? null;
}

function resolveProjects<P extends { id: string; defaultEnvironmentId: string | null }>(projects: P[], environments: EnvironmentSummary[]) {
  return projects.map((project) => ({ ...project, resolvedEnvironment: resolveDefaultEnvironment(project, environments) }));
}

/** Org Store rows hold every project's environments, and a namespace is unique only within its project. */
export function findEnvironment<E extends { projectId: string; namespace: string }>(
  projects: Iterable<{ id: string; slug: string }>,
  environments: Iterable<E>,
  input: { projectSlug?: string; environmentSlug?: string },
) {
  const projectId = [...projects].find((project) => project.slug === input.projectSlug)?.id;
  return [...environments].find((environment) => environment.projectId === projectId && environment.namespace === input.environmentSlug);
}

export function readWorkspace(collections: ReturnType<typeof workspaceCollections>) {
  return resolveProjects([...collections.projects.values()], [...collections.environments.values()]);
}

export async function loadWorkspaceEnvironment(input: EnvironmentBySlug, scope: CollectionScope) {
  const collections = workspaceCollections(input.organizationSlug, scope);
  await Promise.all([preloadCollection(collections.projects), preloadCollection(collections.environments)]);
  const environment = findEnvironment(collections.projects.values(), collections.environments.values(), input);
  if (!environment) throw notFound();
  return environment;
}

export function useWorkspace(organizationSlug: string) {
  const scope = useCollectionScope();
  const collections = workspaceCollections(organizationSlug, scope);
  const projects = useLiveQuery(collections.projects);
  const environments = useLiveQuery(collections.environments);
  const isError = useSyncExternalStore(
    (onChange) => scope.queryClient.getQueryCache().subscribe(onChange),
    () => Object.values(collections).some((collection) => collection.utils.isError),
    () => false,
  );
  const environmentRows = environments.data;
  return {
    projects: resolveProjects(projects.data, environmentRows),
    environments: environmentRows,
    isPending: projects.isLoading || environments.isLoading,
    isError,
    refetch: () => Promise.all(Object.values(collections).map((collection) => collection.utils.refetch())),
  };
}

/** Route lifecycle runs on the server too. Only committed browser navigation stores preferences. */
export async function rememberSelectedOrganization(client: QueryClient, slug: string, initialSlug: string | null, selectOrganization = syncOrganizationSlugServerFn) {
  if (environmentManager.isServer()) return;
  const key = ["organization-preference"];
  try {
    await client.getMutationCache().build(client, {
      scope: { id: "organization-preference" },
      mutationFn: async () => {
        if ((client.getQueryData<string>(key) ?? initialSlug) === slug) return;
        await selectOrganization({ data: { organizationSlug: slug } });
        client.setQueryData(key, slug);
      },
    }).execute(undefined);
  } catch {
    toast.error("Could not remember your selected organization.");
  }
}
