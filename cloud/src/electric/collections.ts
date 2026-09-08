import { snakeCamelMapper, type Row } from "@electric-sql/client";
import {
  electricCollectionOptions,
  type ElectricCollectionConfig,
} from "@tanstack/electric-db-collection";
import {
  BasicIndex,
  createCollection,
} from "@tanstack/react-db";

import { tableSyncUrl } from "#/electric/table-sync-url";
import type { OrganizationTableName } from "#/electric/synced-tables.server";
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
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";
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
type DestructiveVolumeAttemptRow =
  typeof schemaDestructiveVolumeAttempt.$inferSelect;

const dateParser = (value: string) => new Date(value);
const parser = {
  timestamptz: dateParser,
  timestamp: dateParser,
  int8: (value: string) => Number(value),
};

function makeOrganizationCollection<T extends Row<Date>>(input: {
  table: OrganizationTableName;
  organizationSlug: string;
  baseUrl?: string;
  getKey: (row: T) => string | number;
}) {
  const id = `electric:${input.organizationSlug}:${input.table}`;
  const config: ElectricCollectionConfig<T> = {
    id,
    startSync: true,
    ["shapeOptions"]: {
      url: tableSyncUrl(
        input.table,
        { organizationSlug: input.organizationSlug },
        input.baseUrl,
      ),
      columnMapper: snakeCamelMapper(),
      // SAFETY: Electric's parser is row-generic; these handlers are column-type parsers shared across tables.
      parser: parser as typeof parser &
        NonNullable<ElectricCollectionConfig<T>["shapeOptions"]["parser"]>,
    },
    getKey: input.getKey,
    autoIndex: "eager",
    defaultIndexType: BasicIndex,
  };
  return createCollection(electricCollectionOptions(config));
}

function cachedByOrganization<T>(
  create: (organizationSlug: string, baseUrl: string | undefined) => T,
) {
  const cache = new Map<string, T>();
  return (organizationSlug: string, baseUrl?: string) => {
    const existing = cache.get(organizationSlug);
    if (existing) return existing;
    const collection = create(organizationSlug, baseUrl);
    cache.set(organizationSlug, collection);
    return collection;
  };
}

export const getProjectsCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<ProjectRow>({
      table: "project",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
);

export const getEnvironmentsCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<EnvironmentRow>({
      table: "environment",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
);

export const getRawServicesCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<ServiceRow>({
      table: "service",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
);

export const getCanvasPositionsCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<CanvasPositionRow>({
      table: "environment_canvas_node_position",
      organizationSlug,
      baseUrl,
      getKey: (row) => `${row.resourceType}:${row.resourceId}`,
    }),
);

export const getResourceLineagesCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<ResourceLineageRow>({
      table: "resource_lineage",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
);

export const getRawEnvironmentResourcesCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<EnvironmentResourceRow>({
      table: "environment_resource",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
);

export const getEnvironmentDeploymentsCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<EnvironmentDeploymentRow>({
      table: "environment_deployment",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
);

export const getEnvironmentSavedStateRevisionsCollection =
  cachedByOrganization((organizationSlug, baseUrl) =>
    makeOrganizationCollection<EnvironmentSavedStateRevisionRow>({
      table: "environment_saved_state_snapshot",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
  );

export const getEnvironmentNodeConfigSnapshotsCollection =
  cachedByOrganization((organizationSlug, baseUrl) =>
    makeOrganizationCollection<EnvironmentNodeConfigSnapshotRow>({
      table: "environment_node_config_snapshot",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
  );

export const getEnvironmentNodeIntroductionsCollection =
  cachedByOrganization((organizationSlug, baseUrl) =>
    makeOrganizationCollection<EnvironmentNodeIntroductionRow>({
      table: "environment_node_introduction",
      organizationSlug,
      baseUrl,
      getKey: (row) => `${row.nodeType}:${row.nodeId}`,
    }),
  );

export const getDestructiveVolumeAttemptsCollection = cachedByOrganization(
  (organizationSlug, baseUrl) =>
    makeOrganizationCollection<DestructiveVolumeAttemptRow>({
      table: "destructive_volume_attempt",
      organizationSlug,
      baseUrl,
      getKey: (row) => row.id,
    }),
);

export const getVolumeRemovalResultsCollection = cachedByOrganization((organizationSlug, baseUrl) =>
  makeOrganizationCollection<Pick<typeof schemaVolumeRemoveAttempt.$inferSelect, "id" | "organizationId" | "environmentId" | "environmentResourceId" | "status" | "terminalAt">>({
    table: "volume_remove_attempt", organizationSlug, baseUrl, getKey: (row) => row.id,
  }));
