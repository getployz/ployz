import { cachedByCollectionScope, getDbClient } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { compileSavedEnvironmentIntent } from "./saved-intent";
import { BasicIndex, collectionOptions, liveQueryCollectionOptions, type DbClient, eq, useLiveQuery } from "@tanstack/react-db";
import { getEnvironmentsCollection, getProjectsCollection } from "#/collections/collections";
import { plainRowCollection, withoutVirtualProps } from "#/lib/tanstack-db";

export function createEnvironmentDocumentsCollection(client: DbClient, { environments, projects }: {
  environments: ReturnType<typeof getEnvironmentsCollection>;
  projects: ReturnType<typeof getProjectsCollection>;
}) {
  const collection = client.collection(collectionOptions({ ...liveQueryCollectionOptions({
    id: `${environments.id}:documents`,
    query: (q) => q.from({ environment: environments })
      .innerJoin({ project: projects }, ({ environment, project }) => eq(environment.projectId, project.id))
      .fn.select(({ environment, project }) => ({ ...withoutVirtualProps(environment), projectSlug: project.slug,
        compiled: compileSavedEnvironmentIntent({ environmentId: environment.id, intent: environment.intent }),
      })),
    getKey: (document) => document.id,
  }), autoIndex: "eager", defaultIndexType: BasicIndex }));
  return plainRowCollection(collection);
}
export const getEnvironmentDocumentsCollection = cachedByCollectionScope((organizationSlug, scope) =>
  createEnvironmentDocumentsCollection(getDbClient(scope.queryClient), {
    environments: getEnvironmentsCollection(organizationSlug, scope),
    projects: getProjectsCollection(organizationSlug, scope),
  }));

export function useEnvironmentDocument(organizationSlug: string, environmentId: string | null) {
  const collection = getEnvironmentDocumentsCollection(organizationSlug, useCollectionScope());
  return useLiveQuery({ queryKey: ['environment-document', collection.id, environmentId], query: (q) => q.from({ document: collection })
    .where(({ document }) => eq(document.id, environmentId ?? "")).findOne() }).data;
}
