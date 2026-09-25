import type { CollectionName, CollectionRead } from "./read.contract";
import type { OrganizationEnrollmentRow } from "#/modules/machines/enrollment";
import type { ClusterDomainRow } from "#/modules/cluster-domain/cluster-domain";
import type { BuildOrderRow } from "#/modules/deployments/build-order";
import { createChangeCollection } from "#/collections/query-collection";
import { readCollectionServerFn } from "#/collections/read.functions";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import {
  environmentDeployment as schemaEnvironmentDeployment,
  environmentSavedStateSnapshot as schemaEnvironmentSavedStateSnapshot,
} from "#/modules/deployments/tables";
import {
  service as schemaService,
  environmentCanvasNodePosition as schemaEnvironmentCanvasNodePosition,
  resourceLineage as schemaResourceLineage,
  environmentResource as schemaEnvironmentResource,
} from "#/modules/environment-design/tables";
import {
  project as schemaProject,
  environment as schemaEnvironment,
} from "#/modules/project/tables";
import {
  environmentNodeConfigSnapshot as schemaEnvironmentNodeConfigSnapshot,
  environmentNodeIntroduction as schemaEnvironmentNodeIntroduction,
  volumeRemoveAttempt as schemaVolumeRemoveAttempt,
} from "#/modules/runtime/tables";

type ProjectRow = typeof schemaProject.$inferSelect;
type EnvironmentRow = typeof schemaEnvironment.$inferSelect;
type ServiceRow = typeof schemaService.$inferSelect;
type CanvasPositionRow = typeof schemaEnvironmentCanvasNodePosition.$inferSelect;
type ResourceLineageRow = typeof schemaResourceLineage.$inferSelect;
type EnvironmentResourceRow = typeof schemaEnvironmentResource.$inferSelect;
type EnvironmentDeploymentRow = typeof schemaEnvironmentDeployment.$inferSelect;
type EnvironmentSavedStateRevisionRow = Pick<
  typeof schemaEnvironmentSavedStateSnapshot.$inferSelect,
  "id" | "organizationId" | "environmentId"
>;
type EnvironmentNodeConfigSnapshotRow =
  typeof schemaEnvironmentNodeConfigSnapshot.$inferSelect;
type EnvironmentNodeIntroductionRow =
  typeof schemaEnvironmentNodeIntroduction.$inferSelect;
type VolumeRemoveAttemptRow = typeof schemaVolumeRemoveAttempt.$inferSelect;

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

export const getProjectsCollection = changeCollection<ProjectRow>("project", (row) => row.id);
export const getEnvironmentsCollection = changeCollection<EnvironmentRow>("environment", (row) => row.id);
export const getRawServicesCollection = changeCollection<ServiceRow>("service", (row) => row.id);
export const getCanvasPositionsCollection = changeCollection<CanvasPositionRow>(
  "environment_canvas_node_position", (row) => `${row.resourceType}:${row.resourceId}`);
export const getResourceLineagesCollection = changeCollection<ResourceLineageRow>("resource_lineage", (row) => row.id);
export const getRawEnvironmentResourcesCollection = changeCollection<EnvironmentResourceRow>("environment_resource", (row) => row.id);
export const getEnvironmentDeploymentsCollection = changeCollection<EnvironmentDeploymentRow>("environment_deployment", (row) => row.id);
export const getEnvironmentSavedStateRevisionsCollection = changeCollection<EnvironmentSavedStateRevisionRow>(
  "environment_saved_state_snapshot", (row) => row.id);
export const getEnvironmentNodeConfigSnapshotsCollection = changeCollection<EnvironmentNodeConfigSnapshotRow>(
  "environment_node_config_snapshot", (row) => row.id);
export const getEnvironmentNodeIntroductionsCollection = changeCollection<EnvironmentNodeIntroductionRow>(
  "environment_node_introduction", (row) => `${row.nodeType}:${row.nodeId}`);
export const getVolumeRemoveAttemptsCollection = changeCollection<VolumeRemoveAttemptRow>("volume_remove_attempt", (row) => row.id);
export const getOrganizationEnrollmentCollection = changeCollection<OrganizationEnrollmentRow>("organization_enrollment", (row) => row.id);
export const getClusterDomainCollection = changeCollection<ClusterDomainRow>("organization_cluster_domain", (row) => row.id);
export const getBuildOrderCollection = changeCollection<BuildOrderRow>("organization_build_order", (row) => row.id);

export type EnvironmentSummary = Pick<EnvironmentRow, "id" | "projectId" | "organizationId" | "name" | "namespace" | "createdAt">;
export function environmentSummary(row: EnvironmentSummary): EnvironmentSummary {
  const { id, projectId, organizationId, name, namespace, createdAt } = row;
  return { id, projectId, organizationId, name, namespace, createdAt };
}
export type ProjectPreference = { id: string; environmentId: string };

export const getEnvironmentSummariesCollection = changeCollection<EnvironmentSummary>("environment_summary", (row) => row.id);
export const getProjectPreferencesCollection = changeCollection<ProjectPreference>("project_preference", (row) => row.id);

/**
 * Every Org Store table by the name the Organization change stream sends.
 * Not a `get*Collection` export, so the Org Store gate doesn't count it twice.
 */
export const orgStoreTables = {
  project: getProjectsCollection,
  environment: getEnvironmentsCollection,
  environment_summary: getEnvironmentSummariesCollection,
  project_preference: getProjectPreferencesCollection,
  service: getRawServicesCollection,
  resource_lineage: getResourceLineagesCollection,
  environment_resource: getRawEnvironmentResourcesCollection,
  environment_canvas_node_position: getCanvasPositionsCollection,
  environment_deployment: getEnvironmentDeploymentsCollection,
  environment_saved_state_snapshot: getEnvironmentSavedStateRevisionsCollection,
  environment_node_config_snapshot: getEnvironmentNodeConfigSnapshotsCollection,
  environment_node_introduction: getEnvironmentNodeIntroductionsCollection,
  volume_remove_attempt: getVolumeRemoveAttemptsCollection,
  organization_enrollment: getOrganizationEnrollmentCollection,
  organization_cluster_domain: getClusterDomainCollection,
  organization_build_order: getBuildOrderCollection,
} satisfies Record<CollectionName, (organizationSlug: string, scope: CollectionScope) => object>;
