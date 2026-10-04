import type { CollectionName, CollectionRead } from "./read.contract";
import type { OrganizationEnrollmentRow } from "#/modules/machines/enrollment";
import type { ClusterDomainRow } from "#/modules/cluster-domain/cluster-domain";
import type { ServerUpgradeSettingsRow } from "#/modules/server-upgrade/server-upgrade";
import { createChangeCollection } from "#/collections/query-collection";
import { readCollectionServerFn } from "#/collections/read.functions";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import type { environmentCanvasNodePosition } from "#/modules/canvas/tables";
import { canvasPositionKey } from "#/modules/canvas/canvas-positions";

type CanvasPositionRow = typeof environmentCanvasNodePosition.$inferSelect;

/** Every Org Store collection is fed by the Organization change log: a refetch reads only rows changed `since` its cursor. */
function changeCollection<Row extends object>(table: CollectionName, getKey: (row: Row) => string) {
  return cachedByCollectionScope((organizationSlug, scope) => createChangeCollection<Row>({
    queryClient: scope.queryClient,
    queryKey: ["collections", scope.sessionId, scope.userId, organizationSlug, table],
    getKey,
    read: async ({ signal, since }) => {
      // SAFETY: each owner below pairs its literal allowlisted table with that table's database row type.
      return await readCollectionServerFn({ data: { table, organizationSlug, userId: scope.userId, since }, signal }) as CollectionRead<Row>;
    },
  }));
}

export const getCanvasPositionsCollection = changeCollection<CanvasPositionRow>("environment_canvas_node_position", canvasPositionKey);
export const getOrganizationEnrollmentCollection = changeCollection<OrganizationEnrollmentRow>("organization_enrollment", (row) => row.id);
export const getClusterDomainCollection = changeCollection<ClusterDomainRow>("organization_cluster_domain", (row) => row.id);
/** The Organization's Server upgrade settings: no row reads as the defaults. */
export const getServerUpgradeSettingsCollection = changeCollection<ServerUpgradeSettingsRow>("organization_server_upgrades", (row) => row.id);

/**
 * Every Org Store table by the name the Organization change stream sends.
 * Not a `get*Collection` export, so the Org Store gate doesn't count it twice.
 */
export const orgStoreTables = {
  environment_canvas_node_position: getCanvasPositionsCollection,
  organization_enrollment: getOrganizationEnrollmentCollection,
  organization_cluster_domain: getClusterDomainCollection,
  organization_server_upgrades: getServerUpgradeSettingsCollection,
} satisfies Record<CollectionName, (organizationSlug: string, scope: CollectionScope) => object>;
