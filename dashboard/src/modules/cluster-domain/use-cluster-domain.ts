import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { getClusterDomainCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { ClusterDomainRow } from "#/modules/cluster-domain/cluster-domain";

/** The Organization's Cluster Domain row, or null while none is reserved. Needs no connected Server. */
export function useClusterDomain(organizationSlug: string): ClusterDomainRow | null {
  const { data: rows } = useLiveSuspenseQuery(getClusterDomainCollection(organizationSlug, useCollectionScope()));
  return rows[0] ?? null;
}

/** The Organization's Cluster Domain name, or null while none is reserved. */
export function useClusterDomainName(organizationSlug: string): string | null {
  return useClusterDomain(organizationSlug)?.name ?? null;
}
