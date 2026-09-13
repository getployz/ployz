import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { compileEnvironmentIntent } from "@ployz/sdk/config";
import { createLiveQueryCollection, eq, useLiveQuery } from "@tanstack/react-db";
import { getEnvironmentsCollection, getProjectsCollection } from "#/collections/collections";
import { plainRowCollection, withoutVirtualProps } from "#/lib/tanstack-db";

export function createEnvironmentDocumentsCollection(organizationSlug: string, { environments, projects }: {
  environments: ReturnType<typeof getEnvironmentsCollection>;
  projects: ReturnType<typeof getProjectsCollection>;
}) {
  return plainRowCollection(createLiveQueryCollection({
    id: `collections:${organizationSlug}:environment-documents`,
    query: (q) => q.from({ environment: environments })
      .innerJoin({ project: projects }, ({ environment, project }) => eq(environment.projectId, project.id))
      .fn.select(({ environment, project }) => ({ ...withoutVirtualProps(environment), projectSlug: project.slug,
        compiled: compileEnvironmentIntent(environment.id, environment.intent),
      })),
    getKey: (document) => document.id,
  }));
}
export const getEnvironmentDocumentsCollection = cachedByCollectionScope((organizationSlug, scope) =>
  createEnvironmentDocumentsCollection(organizationSlug, {
    environments: getEnvironmentsCollection(organizationSlug, scope),
    projects: getProjectsCollection(organizationSlug, scope),
  }));

export function useEnvironmentDocument(organizationSlug: string, environmentId: string | null) {
  const collection = getEnvironmentDocumentsCollection(organizationSlug, useCollectionScope());
  return useLiveQuery((q) => q.from({ document: collection })
    .where(({ document }) => eq(document.id, environmentId ?? "")).findOne(), [collection, environmentId]).data;
}
