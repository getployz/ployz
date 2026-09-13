import { toast } from "sonner";
import type { EnvironmentBySlug } from "./workspace-schemas";
import { queryOptions, type QueryClient } from "@tanstack/react-query";
import { hasPublicErrorCode } from "#/lib/public-error";
import {
  getEnvironmentBySlugServerFn,
  getOrganizationStateServerFn,
  getProjectBySlugServerFn,
  listEnvironmentsServerFn,
  listProjectsServerFn,
  resolvePreferredEnvironmentServerFn,
  selectEnvironmentServerFn,
} from "./workspace-functions";

const DEFAULT_CLIENT_QUERY_RETRY_COUNT = 3;

function retryWorkspaceQuery(failureCount: number, cause: unknown) {
  return (
    !hasPublicErrorCode(cause, "NOT_FOUND") &&
    failureCount < DEFAULT_CLIENT_QUERY_RETRY_COUNT
  );
}

export const organizationKeys = {
  all: ["organization"] as const,
  state: (organizationSlug?: string | null) =>
    [...organizationKeys.all, organizationSlug ?? null, "state"] as const,
};

export function organizationStateQueryOptions(organizationSlug?: string | null) {
  return queryOptions({
    queryKey: organizationKeys.state(organizationSlug),
    queryFn: ({ signal }) =>
      getOrganizationStateServerFn({
        data: organizationSlug === undefined || organizationSlug === null
          ? {}
          : { organizationSlug },
        signal,
      }),
  });
}

export const projectKeys = {
  all: ["projects"] as const,
  org: (organizationSlug: string) =>
    [...projectKeys.all, organizationSlug] as const,
  list: (organizationSlug: string) =>
    [...projectKeys.org(organizationSlug), "list"] as const,
  detail: (organizationSlug: string, projectSlug: string) =>
    [...projectKeys.org(organizationSlug), "detail", projectSlug] as const,
};

export function projectListQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: projectKeys.list(organizationSlug),
    queryFn: ({ signal }) =>
      listProjectsServerFn({ data: { organizationSlug }, signal }),
  });
}

export function projectBySlugQueryOptions(
  organizationSlug: string,
  projectSlug: string,
) {
  return queryOptions({
    queryKey: projectKeys.detail(organizationSlug, projectSlug),
    queryFn: ({ signal }) =>
      getProjectBySlugServerFn({
        data: { organizationSlug, projectSlug },
        signal,
      }),
    retry: retryWorkspaceQuery,
  });
}

export const environmentKeys = {
  all: ["environments"] as const,
  project: (organizationSlug: string, projectSlug: string) =>
    [...environmentKeys.all, organizationSlug, projectSlug] as const,
  list: (organizationSlug: string, projectSlug: string) =>
    [...environmentKeys.project(organizationSlug, projectSlug), "list"] as const,
  detail: (
    organizationSlug: string,
    projectSlug: string,
    environmentSlug: string,
  ) =>
    [
      ...environmentKeys.project(organizationSlug, projectSlug),
      "detail",
      environmentSlug,
    ] as const,
  preferred: (organizationSlug: string, projectSlug: string) =>
    [...environmentKeys.project(organizationSlug, projectSlug), "preferred"] as const,
};

export function environmentListQueryOptions(
  organizationSlug: string,
  projectSlug: string,
) {
  return queryOptions({
    queryKey: environmentKeys.list(organizationSlug, projectSlug),
    queryFn: ({ signal }) =>
      listEnvironmentsServerFn({
        data: { organizationSlug, projectSlug },
        signal,
      }),
  });
}

export function environmentBySlugQueryOptions(
  organizationSlug: string,
  projectSlug: string,
  environmentSlug: string,
) {
  return queryOptions({
    queryKey: environmentKeys.detail(
      organizationSlug,
      projectSlug,
      environmentSlug,
    ),
    queryFn: ({ signal }) =>
      getEnvironmentBySlugServerFn({
        data: { organizationSlug, projectSlug, environmentSlug },
        signal,
      }),
    retry: retryWorkspaceQuery,
  });
}

export function preferredEnvironmentQueryOptions(
  organizationSlug: string,
  projectSlug: string,
) {
  return queryOptions({
    queryKey: environmentKeys.preferred(organizationSlug, projectSlug),
    queryFn: ({ signal }) =>
      resolvePreferredEnvironmentServerFn({
        data: { organizationSlug, projectSlug },
        signal,
      }),
    retry: retryWorkspaceQuery,
  });
}

/** Called by Router only when an environment route is entered or retained, never by preload. */
export async function rememberSelectedEnvironment(queryClient: QueryClient, input: EnvironmentBySlug, selectEnvironment = selectEnvironmentServerFn) {
  const { organizationSlug, projectSlug, environmentSlug } = input;
  const environment = queryClient.getQueryData(
    environmentBySlugQueryOptions(organizationSlug, projectSlug, environmentSlug).queryKey,
  );
  if (!environment) return;
  const preferredKey = environmentKeys.preferred(organizationSlug, projectSlug);
  if (queryClient.getQueryData<{ id: string }>(preferredKey)?.id === environment.id) return;
  const projectsKey = projectListQueryOptions(organizationSlug).queryKey;
  await Promise.all([
    queryClient.cancelQueries({ queryKey: preferredKey, exact: true }),
    queryClient.cancelQueries({ queryKey: projectsKey, exact: true }),
  ]);
  queryClient.setQueryData(preferredKey, environment);
  queryClient.setQueryData(projectsKey, (projects) => projects?.map((project) =>
    project.id === environment.projectId
      ? { ...project, userDefaultEnvironmentId: environment.id, resolvedEnvironment: environment }
      : project,
  ));
  try {
    // TanStack serializes writes for this project so rapid switching persists the final selection last.
    await queryClient.getMutationCache().build(queryClient, {
      scope: { id: `environment-preference:${environment.projectId}` },
      mutationFn: () => selectEnvironment({ data: input }),
    }).execute(undefined);
  } catch {
    toast.error("Could not remember your selected environment.");
  }
}
