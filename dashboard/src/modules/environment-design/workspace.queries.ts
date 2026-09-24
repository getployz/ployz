import { toast } from "sonner";
import { environmentManager, queryOptions, type QueryClient } from "@tanstack/react-query";
import { useLiveQuery } from "@tanstack/react-db";
import { useSyncExternalStore } from "react";
import { notFound } from "@tanstack/react-router";
import { getProjectsCollection, getEnvironmentSummariesCollection, getProjectPreferencesCollection, type EnvironmentSummary } from "#/collections/collections";
import { preloadCollection } from "#/collections/query-collection";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { EnvironmentBySlug } from "./workspace-schemas";
import { getOrganizationStateServerFn, selectEnvironmentServerFn, syncOrganizationSlugServerFn } from "./workspace-functions";

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
    preferences: getProjectPreferencesCollection(organizationSlug, scope),
  };
}

export async function preloadWorkspace(organizationSlug: string, scope: CollectionScope) {
  const collections = workspaceCollections(organizationSlug, scope);
  await Promise.all(Object.values(collections).map(preloadCollection));
  return collections;
}

function resolveProjects(
  projects: Array<{ id: string; organizationId: string; name: string; slug: string }>,
  environments: EnvironmentSummary[],
  preferences: Array<{ id: string; environmentId: string }>,
) {
  const ordered = [...environments].sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime());
  return projects.map((project) => {
    const preferredId = preferences.find((preference) => preference.id === project.id)?.environmentId;
    const projectEnvironments = ordered.filter((environment) => environment.projectId === project.id);
    return { ...project, resolvedEnvironment: projectEnvironments.find((environment) => environment.id === preferredId) ?? projectEnvironments[0] ?? null };
  });
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
  return resolveProjects([...collections.projects.values()], [...collections.environments.values()], [...collections.preferences.values()]);
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
  const preferences = useLiveQuery(collections.preferences);
  const isError = useSyncExternalStore(
    (onChange) => scope.queryClient.getQueryCache().subscribe(onChange),
    () => Object.values(collections).some((collection) => collection.utils.isError),
    () => false,
  );
  const environmentRows = environments.data;
  return {
    projects: resolveProjects(projects.data, environmentRows, preferences.data),
    environments: environmentRows,
    isPending: projects.isLoading || environments.isLoading || preferences.isLoading,
    isError,
    refetch: () => Promise.all(Object.values(collections).map((collection) => collection.utils.refetch())),
  };
}

/** Selection is already represented by the URL. Only confirmed writes change the saved preference. */
export async function rememberSelectedEnvironment(scope: CollectionScope, input: EnvironmentBySlug, selectEnvironment = selectEnvironmentServerFn) {
  const { queryClient } = scope;
  const collections = workspaceCollections(input.organizationSlug, scope);
  const environment = findEnvironment(collections.projects.values(), collections.environments.values(), input);
  if (!environment) return;
  try {
    await queryClient.getMutationCache().build(queryClient, {
      scope: { id: `environment-preference:${environment.projectId}` },
      mutationFn: async () => {
        // Check inside the serialized mutation: an earlier selection may still be in flight.
        if (collections.preferences.get(environment.projectId)?.environmentId === environment.id) return;
        const saved = await selectEnvironment({ data: input });
        await collections.preferences.writeCommitted({ id: saved.projectId, environmentId: saved.id });
      },
    }).execute(undefined);
  } catch {
    toast.error("Could not remember your selected environment.");
  }
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
