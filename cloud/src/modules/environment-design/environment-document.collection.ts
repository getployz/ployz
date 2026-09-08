import { compileEnvironmentIntent } from "@ployz/sdk/config";
import { createLiveQueryCollection, eq, useLiveQuery } from "@tanstack/react-db";
import { getEnvironmentsCollection, getProjectsCollection } from "#/electric/collections";
import { plainRowCollection, withoutVirtualProps } from "#/lib/tanstack-db";

function createEnvironmentDocumentsCollection(organizationSlug: string) {
  const environments = getEnvironmentsCollection(organizationSlug);
  const projects = getProjectsCollection(organizationSlug);
  return plainRowCollection(createLiveQueryCollection({
    id: `electric:${organizationSlug}:environment-documents`, startSync: true,
    query: (q) => q.from({ environment: environments })
      .innerJoin({ project: projects }, ({ environment, project }) => eq(environment.projectId, project.id))
      .fn.select(({ environment, project }) => ({ ...withoutVirtualProps(environment), projectSlug: project.slug,
        compiled: compileEnvironmentIntent(environment.id, environment.intent),
      })),
    getKey: (document) => document.id,
  }));
}
const documents = new Map<string, ReturnType<typeof createEnvironmentDocumentsCollection>>();
export function getEnvironmentDocumentsCollection(organizationSlug: string) {
  const existing = documents.get(organizationSlug);
  if (existing) return existing;
  const collection = createEnvironmentDocumentsCollection(organizationSlug);
  documents.set(organizationSlug, collection);
  return collection;
}

export function useEnvironmentDocument(organizationSlug: string, environmentId: string | null) {
  const collection = getEnvironmentDocumentsCollection(organizationSlug);
  return useLiveQuery((q) => q.from({ document: collection })
    .where(({ document }) => eq(document.id, environmentId ?? "")).findOne(), [collection, environmentId]).data;
}
