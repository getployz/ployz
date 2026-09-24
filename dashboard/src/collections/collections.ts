import type { CollectionReadInput } from "./read.contract";
import type { OrganizationEnrollmentRow } from "#/modules/machines/enrollment";
import { createApiCollection } from "#/collections/query-collection";
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

function collectionReadOptions<Row>(table: CollectionReadInput["table"], organizationSlug: string, scope: CollectionScope) {
  return {
    queryClient: scope.queryClient,
    queryKey: ["collections", scope.sessionId, scope.userId, organizationSlug, table],
    queryFn: async ({ signal }: { signal: AbortSignal }) => {
      // SAFETY: each owner below pairs its literal allowlisted table with that table's database row type.
      return await readCollectionServerFn({ data: { table, organizationSlug, userId: scope.userId }, signal }) as Row[];
    },
  };
}

export const getProjectsCollection = cachedByCollectionScope(
  (organizationSlug, scope) => createApiCollection<ProjectRow>({
    ...collectionReadOptions<ProjectRow>("project", organizationSlug, scope),
    getKey: (row) => row.id,
  }),
);

export const getEnvironmentsCollection = cachedByCollectionScope(
  (organizationSlug, scope) => createApiCollection<EnvironmentRow>({
    ...collectionReadOptions<EnvironmentRow>("environment", organizationSlug, scope),
    getKey: (row) => row.id,
  }),
);

export const getRawServicesCollection = cachedByCollectionScope(
  (organizationSlug, scope) => createApiCollection<ServiceRow>({
    ...collectionReadOptions<ServiceRow>("service", organizationSlug, scope),
    getKey: (row) => row.id,
  }),
);

export const getCanvasPositionsCollection = cachedByCollectionScope(
  (organizationSlug, scope) => createApiCollection<CanvasPositionRow>({
    ...collectionReadOptions<CanvasPositionRow>("environment_canvas_node_position", organizationSlug, scope),
    getKey: (row) => `${row.resourceType}:${row.resourceId}`,
  }),
);

export const getResourceLineagesCollection = cachedByCollectionScope(
  (organizationSlug, scope) => createApiCollection<ResourceLineageRow>({
    ...collectionReadOptions<ResourceLineageRow>("resource_lineage", organizationSlug, scope),
    getKey: (row) => row.id,
  }),
);

export const getRawEnvironmentResourcesCollection = cachedByCollectionScope(
  (organizationSlug, scope) => createApiCollection<EnvironmentResourceRow>({
    ...collectionReadOptions<EnvironmentResourceRow>("environment_resource", organizationSlug, scope),
    getKey: (row) => row.id,
  }),
);

export const getEnvironmentDeploymentsCollection = cachedByCollectionScope(
  (organizationSlug, scope) =>
    createApiCollection<EnvironmentDeploymentRow>({
    ...collectionReadOptions<EnvironmentDeploymentRow>("environment_deployment", organizationSlug, scope),
    refetchInterval: 2_000,
    getKey: (row) => row.id,
    }),
);

export const getEnvironmentSavedStateRevisionsCollection =
  cachedByCollectionScope((organizationSlug, scope) =>
    createApiCollection<EnvironmentSavedStateRevisionRow>({
    ...collectionReadOptions<EnvironmentSavedStateRevisionRow>("environment_saved_state_snapshot", organizationSlug, scope),
    getKey: (row) => row.id,
    }),
  );

export const getEnvironmentNodeConfigSnapshotsCollection =
  cachedByCollectionScope((organizationSlug, scope) =>
    createApiCollection<EnvironmentNodeConfigSnapshotRow>({
    ...collectionReadOptions<EnvironmentNodeConfigSnapshotRow>("environment_node_config_snapshot", organizationSlug, scope),
    getKey: (row) => row.id,
    }),
  );

export const getEnvironmentNodeIntroductionsCollection =
  cachedByCollectionScope((organizationSlug, scope) =>
    createApiCollection<EnvironmentNodeIntroductionRow>({
    ...collectionReadOptions<EnvironmentNodeIntroductionRow>("environment_node_introduction", organizationSlug, scope),
    getKey: (row) => `${row.nodeType}:${row.nodeId}`,
    }),
  );

export const getVolumeRemoveAttemptsCollection = cachedByCollectionScope(
  (organizationSlug, scope) =>
    createApiCollection<VolumeRemoveAttemptRow>({
    ...collectionReadOptions<VolumeRemoveAttemptRow>("volume_remove_attempt", organizationSlug, scope),
    getKey: (row) => row.id,
    }),
);

export const getOrganizationEnrollmentCollection = cachedByCollectionScope((organizationSlug, scope) =>
  createApiCollection<OrganizationEnrollmentRow>({
    ...collectionReadOptions<OrganizationEnrollmentRow>("organization_enrollment", organizationSlug, scope),
    getKey: (row) => row.id,
  }));

export type EnvironmentSummary = Pick<EnvironmentRow, "id" | "projectId" | "organizationId" | "name" | "namespace" | "createdAt">;
export function environmentSummary(row: EnvironmentSummary): EnvironmentSummary {
  const { id, projectId, organizationId, name, namespace, createdAt } = row;
  return { id, projectId, organizationId, name, namespace, createdAt };
}
export type ProjectPreference = { id: string; environmentId: string };

export const getEnvironmentSummariesCollection = cachedByCollectionScope((organizationSlug, scope) =>
  createApiCollection<EnvironmentSummary>({
    ...collectionReadOptions<EnvironmentSummary>("environment_summary", organizationSlug, scope),
    getKey: row => row.id,
  }));

export const getProjectPreferencesCollection = cachedByCollectionScope((organizationSlug, scope) =>
  createApiCollection<ProjectPreference>({
    ...collectionReadOptions<ProjectPreference>("project_preference", organizationSlug, scope),
    getKey: row => row.id,
  }));
