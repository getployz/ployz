import type { OrgCollection } from "./change-log/declare";
import type { CollectionName, CollectionRead } from "./read.contract";
import { createChangeCollection } from "#/collections/query-collection";
import { readCollectionServerFn } from "#/collections/read.functions";
import { cachedByCollectionScope, collectionKey, type CollectionScope } from "#/collections/scope";
import { canvasPositions } from "#/modules/canvas/canvas.collection";
import { clusterDomain } from "#/modules/cluster-domain/cluster-domain.collection";
import { clusterName } from "#/modules/cluster-name/cluster-name.collection";
import { organizationEnrollment } from "#/modules/machines/enrollment.collection";
import { serverUpgradeSettings } from "#/modules/server-upgrade/server-upgrade.collection";
import { supportAccess } from "#/modules/support-access/support-access.collection";

/** Every Org Store collection is fed by the Organization change log: a refetch reads only rows changed `since` its cursor. */
function changeCollection<Name extends CollectionName, Row extends object>(declaration: OrgCollection<Name, Row>) {
  const table: CollectionName = declaration.name;
  return cachedByCollectionScope((organizationSlug, scope) => createChangeCollection<Row>({
    queryClient: scope.queryClient,
    queryKey: collectionKey(scope, organizationSlug, table),
    getKey: declaration.key,
    read: async ({ signal, since }) => {
      // SAFETY: each declaration pairs its allowlisted table name with the row type its projection reads.
      return await readCollectionServerFn({ data: { table, organizationSlug, userId: scope.userId, since }, signal }) as CollectionRead<Row>;
    },
  }));
}

export const getCanvasPositionsCollection = changeCollection(canvasPositions);
export const getOrganizationEnrollmentCollection = changeCollection(organizationEnrollment);
export const getClusterDomainCollection = changeCollection(clusterDomain);
export const getClusterNameCollection = changeCollection(clusterName);
export const getServerUpgradeSettingsCollection = changeCollection(serverUpgradeSettings);
export const getSupportAccessCollection = changeCollection(supportAccess);

/**
 * Every Org Store table by the name the Organization change stream sends.
 * Not a `get*Collection` export, so the Org Store gate doesn't count it twice.
 */
export const orgStoreTables = {
  [canvasPositions.name]: getCanvasPositionsCollection,
  [organizationEnrollment.name]: getOrganizationEnrollmentCollection,
  [clusterDomain.name]: getClusterDomainCollection,
  [clusterName.name]: getClusterNameCollection,
  [serverUpgradeSettings.name]: getServerUpgradeSettingsCollection,
  [supportAccess.name]: getSupportAccessCollection,
} satisfies Record<CollectionName, (organizationSlug: string, scope: CollectionScope) => object>;
