import { useLiveQuery } from "@tanstack/react-db";
import { getRawServicesCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";

/**
 * A node's name by lineage: its service in `environmentId` when there is one, else in any Environment (a Live Node
 * belongs to an ancestor), else its Volume's name, else "a node".
 */
export function useLineageNames(organizationSlug: string) {
  const { data: services } = useLiveQuery(getRawServicesCollection(organizationSlug, useCollectionScope()));
  const environments = useEnvironmentDocuments(organizationSlug);
  return (lineage: string, environmentId?: string) =>
    services.find((row) => row.lineageId === lineage && row.environmentId === environmentId)?.name
      ?? services.find((row) => row.lineageId === lineage)?.name
      ?? environments.flatMap((environment) => environment.intent.volumes).find((node) => node.resourceLineageId === lineage)?.name
      ?? "a node";
}
