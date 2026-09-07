import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import {
  environmentCanvasNodePosition,
  environmentResource,
  environmentVariableGroup,
  resourceLineage,
  serviceVariableGroupAttachment,
  serviceVolumeAttachment,
  variableGroupLineage,
} from "#/modules/environment-design/tables";
import { environment, project } from "#/modules/project/tables";
import {
  organizationIdForEnvironment,
  organizationIdForProject,
} from "#/db/scope-values.server";
import { Database } from "#/server/database.server";
import {
  createResourceLineage,
  createVariableGroupLineage,
} from "./authoring-repository.server";
import { decodeStrict } from "./schema";
import {
  variableGroupResourceRecordSchema,
  volumeResourceRecordSchema,
  type VolumeAttachmentSummary,
} from "./resources";
import { listVariablesForGroups } from "./variable-repository.server";

const resourceColumns = {
  id: environmentResource.id,
  projectId: environmentResource.projectId,
  environmentId: environmentResource.environmentId,
  lineageId: environmentResource.lineageId,
  implementationType: environmentResource.implementationType,
  variableGroupId: environmentResource.variableGroupId,
  name: environmentResource.name,
  slug: environmentResource.slug,
  deletedAt: environmentResource.deletedAt,
  createdAt: environmentResource.createdAt,
  updatedAt: environmentResource.updatedAt,
};

const resourceLineageColumns = {
  id: resourceLineage.id,
  projectId: resourceLineage.projectId,
  canonicalName: resourceLineage.canonicalName,
  canonicalSlug: resourceLineage.canonicalSlug,
  createdAt: resourceLineage.createdAt,
  updatedAt: resourceLineage.updatedAt,
};

const variableGroupColumns = {
  id: environmentVariableGroup.id,
  projectId: environmentVariableGroup.projectId,
  environmentId: environmentVariableGroup.environmentId,
  lineageId: environmentVariableGroup.lineageId,
  name: environmentVariableGroup.name,
  slug: environmentVariableGroup.slug,
  createdAt: environmentVariableGroup.createdAt,
  updatedAt: environmentVariableGroup.updatedAt,
};

const canvasPositionColumns = {
  id: environmentCanvasNodePosition.id,
  environmentId: environmentCanvasNodePosition.environmentId,
  resourceType: environmentCanvasNodePosition.resourceType,
  resourceId: environmentCanvasNodePosition.resourceId,
  x: environmentCanvasNodePosition.x,
  y: environmentCanvasNodePosition.y,
  createdAt: environmentCanvasNodePosition.createdAt,
  updatedAt: environmentCanvasNodePosition.updatedAt,
};

type VariableGroupRow = {
  readonly resource: Omit<
    typeof environmentResource.$inferSelect,
    "organizationId"
  >;
  readonly lineage: Omit<typeof resourceLineage.$inferSelect, "organizationId">;
  readonly variableGroup: Omit<
    typeof environmentVariableGroup.$inferSelect,
    "organizationId"
  >;
  readonly canvasPosition: Omit<
    typeof environmentCanvasNodePosition.$inferSelect,
    "organizationId"
  > | null;
  readonly projectSlug: string;
  readonly environmentSlug: string;
};

type VolumeRow = Omit<VariableGroupRow, "variableGroup">;

const loadVariableGroupConsumerCounts = Effect.fn(
  "EnvironmentDesign.loadVariableGroupConsumerCounts",
)(function* (variableGroupIds: readonly string[]) {
  if (variableGroupIds.length === 0) return new Map<string, number>();
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      variableGroupId: serviceVariableGroupAttachment.variableGroupId,
      serviceId: serviceVariableGroupAttachment.serviceId,
    })
    .from(serviceVariableGroupAttachment)
    .where(
      inArray(serviceVariableGroupAttachment.variableGroupId, [
        ...variableGroupIds,
      ]),
    );
  const servicesByGroup = new Map<string, Set<string>>();
  for (const row of rows) {
    const services = servicesByGroup.get(row.variableGroupId) ?? new Set();
    services.add(row.serviceId);
    servicesByGroup.set(row.variableGroupId, services);
  }
  return new Map(
    [...servicesByGroup].map(([variableGroupId, services]) => [
      variableGroupId,
      services.size,
    ]),
  );
});

const projectVariableGroupRows = Effect.fn(
  "EnvironmentDesign.projectVariableGroupRows",
)(function* (rows: readonly VariableGroupRow[]) {
  const variableGroupIds = rows.map((row) => row.variableGroup.id);
  const [variablesByGroup, consumersByGroup] = yield* Effect.all(
    [
      listVariablesForGroups(variableGroupIds),
      loadVariableGroupConsumerCounts(variableGroupIds),
    ],
    { concurrency: "unbounded" },
  );
  return rows.map((row) => {
    const variables = variablesByGroup.get(row.variableGroup.id) ?? [];
    return decodeStrict(variableGroupResourceRecordSchema, {
      ...row,
      variables,
      exports: variables.flatMap((variable) =>
        variable.exported
          ? [{ key: variable.key, value: variable.value, variableId: variable.id }]
          : [],
      ),
      consumerCount: consumersByGroup.get(row.variableGroup.id) ?? 0,
    });
  });
});

const baseVariableGroupQuery = Effect.fn(
  "EnvironmentDesign.baseVariableGroupQuery",
)(function* () {
  const database = yield* Database;
  return database.drizzle
    .select({
      resource: resourceColumns,
      lineage: resourceLineageColumns,
      variableGroup: variableGroupColumns,
      canvasPosition: canvasPositionColumns,
      projectSlug: project.slug,
      environmentSlug: environment.namespace,
    })
    .from(environmentResource)
    .innerJoin(environment, eq(environmentResource.environmentId, environment.id))
    .innerJoin(project, eq(environment.projectId, project.id))
    .innerJoin(resourceLineage, eq(environmentResource.lineageId, resourceLineage.id))
    .innerJoin(
      environmentVariableGroup,
      eq(environmentResource.variableGroupId, environmentVariableGroup.id),
    )
    .leftJoin(
      environmentCanvasNodePosition,
      and(
        eq(environmentCanvasNodePosition.resourceType, "variable_group"),
        eq(environmentCanvasNodePosition.resourceId, environmentResource.id),
      ),
    );
});

export const getVariableGroupResource = Effect.fn(
  "EnvironmentDesign.getVariableGroupResource",
)(function* (environmentId: string, resourceId: string) {
  const query = yield* baseVariableGroupQuery();
  const rows = yield* query
    .where(
      and(
        eq(environmentResource.environmentId, environmentId),
        eq(environmentResource.id, resourceId),
        eq(environmentResource.implementationType, "variable_group"),
      ),
    )
    .limit(1);
  const records = yield* projectVariableGroupRows(rows);
  return records[0] ?? null;
});

export const getVariableGroupConsumerCount = Effect.fn(
  "EnvironmentDesign.getVariableGroupConsumerCount",
)(function* (variableGroupId: string) {
  const counts = yield* loadVariableGroupConsumerCounts([variableGroupId]);
  return counts.get(variableGroupId) ?? 0;
});

export const getResourceIdentity = Effect.fn(
  "EnvironmentDesign.getResourceIdentity",
)(function* (environmentId: string, resourceId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      id: environmentResource.id,
      implementationType: environmentResource.implementationType,
    })
    .from(environmentResource)
    .where(
      and(
        eq(environmentResource.environmentId, environmentId),
        eq(environmentResource.id, resourceId),
      ),
    )
    .limit(1);
  return rows[0] ?? null;
});

const deleteResourceLineage = Effect.fn(
  "EnvironmentDesign.deleteResourceLineage",
)(function* (lineageId: string) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(resourceLineage)
    .where(eq(resourceLineage.id, lineageId));
});

const deleteVariableGroupLineage = Effect.fn(
  "EnvironmentDesign.deleteVariableGroupLineage",
)(function* (lineageId: string) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(variableGroupLineage)
    .where(eq(variableGroupLineage.id, lineageId));
});

export const createVariableGroupAggregate = Effect.fn(
  "EnvironmentDesign.createVariableGroupAggregate",
)(function* (input: {
  readonly projectId: string;
  readonly environmentId: string;
  readonly name: string;
  readonly slug: string;
  readonly x: number;
  readonly y: number;
}) {
  const database = yield* Database;
  const resourceIdentity = yield* createResourceLineage(input);
  const groupIdentity = yield* createVariableGroupLineage(input);
  if (resourceIdentity === null || groupIdentity === null) {
    if (resourceIdentity !== null) yield* deleteResourceLineage(resourceIdentity.id);
    if (groupIdentity !== null) yield* deleteVariableGroupLineage(groupIdentity.id);
    return null;
  }
  const groups = yield* database.drizzle
    .insert(environmentVariableGroup)
    .values({
      organizationId: organizationIdForProject(input.projectId),
      projectId: input.projectId,
      environmentId: input.environmentId,
      lineageId: groupIdentity.id,
      name: input.name,
      slug: input.slug,
    })
    .onConflictDoNothing()
    .returning(variableGroupColumns);
  const group = groups[0];
  if (group === undefined) {
    yield* deleteResourceLineage(resourceIdentity.id);
    yield* deleteVariableGroupLineage(groupIdentity.id);
    return null;
  }
  const resources = yield* database.drizzle
    .insert(environmentResource)
    .values({
      organizationId: organizationIdForProject(input.projectId),
      projectId: input.projectId,
      environmentId: input.environmentId,
      lineageId: resourceIdentity.id,
      implementationType: "variable_group",
      variableGroupId: group.id,
      name: input.name,
      slug: input.slug,
    })
    .onConflictDoNothing()
    .returning(resourceColumns);
  const resource = resources[0];
  if (resource === undefined) {
    yield* database.drizzle
      .delete(environmentVariableGroup)
      .where(eq(environmentVariableGroup.id, group.id));
    yield* deleteResourceLineage(resourceIdentity.id);
    yield* deleteVariableGroupLineage(groupIdentity.id);
    return null;
  }
  yield* database.drizzle.insert(environmentCanvasNodePosition).values({
    organizationId: organizationIdForEnvironment(input.environmentId),
    environmentId: input.environmentId,
    resourceType: "variable_group",
    resourceId: resource.id,
    x: Math.round(input.x),
    y: Math.round(input.y),
  });
  return resource.id;
});

export const updateVariableGroupName = Effect.fn(
  "EnvironmentDesign.updateVariableGroupName",
)(function* (resourceId: string, variableGroupId: string, name: string) {
  const database = yield* Database;
  const resources = yield* database.drizzle
    .update(environmentResource)
    .set({ name, updatedAt: new Date() })
    .where(eq(environmentResource.id, resourceId))
    .returning({ id: environmentResource.id });
  if (resources[0] === undefined) return null;
  yield* database.drizzle
    .update(environmentVariableGroup)
    .set({ name, updatedAt: new Date() })
    .where(eq(environmentVariableGroup.id, variableGroupId));
  return resources[0];
});

export const deleteVariableGroupAggregate = Effect.fn(
  "EnvironmentDesign.deleteVariableGroupAggregate",
)(function* (resourceId: string, variableGroupId: string) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(environmentCanvasNodePosition)
    .where(
      and(
        eq(environmentCanvasNodePosition.resourceType, "variable_group"),
        eq(environmentCanvasNodePosition.resourceId, resourceId),
      ),
    );
  yield* database.drizzle
    .delete(environmentResource)
    .where(eq(environmentResource.id, resourceId));
  yield* database.drizzle
    .delete(environmentVariableGroup)
    .where(eq(environmentVariableGroup.id, variableGroupId));
  return { deletedId: resourceId };
});

const loadVolumeAttachments = Effect.fn(
  "EnvironmentDesign.loadVolumeAttachments",
)(function* (resourceIds: readonly string[]) {
  if (resourceIds.length === 0) {
    return new Map<string, VolumeAttachmentSummary[]>();
  }
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      volumeResourceId: serviceVolumeAttachment.volumeResourceId,
      serviceId: serviceVolumeAttachment.serviceId,
      mountPath: serviceVolumeAttachment.mountPath,
    })
    .from(serviceVolumeAttachment)
    .where(inArray(serviceVolumeAttachment.volumeResourceId, [...resourceIds]));
  const byResource = new Map<string, VolumeAttachmentSummary[]>();
  for (const row of rows) {
    const attachments = byResource.get(row.volumeResourceId) ?? [];
    attachments.push({ serviceId: row.serviceId, mountPath: row.mountPath });
    byResource.set(row.volumeResourceId, attachments);
  }
  return byResource;
});

const projectVolumeRows = Effect.fn("EnvironmentDesign.projectVolumeRows")(
  function* (rows: readonly VolumeRow[]) {
    const attachments = yield* loadVolumeAttachments(
      rows.map((row) => row.resource.id),
    );
    return rows.map((row) => {
      const resourceAttachments = [
        ...(attachments.get(row.resource.id) ?? []),
      ].sort((left, right) => left.mountPath.localeCompare(right.mountPath));
      return decodeStrict(volumeResourceRecordSchema, {
        ...row,
        attachments: resourceAttachments,
        consumerCount: resourceAttachments.length,
        runtimeStatus: null,
      });
    });
  },
);

const baseVolumeQuery = Effect.fn("EnvironmentDesign.baseVolumeQuery")(
  function* () {
    const database = yield* Database;
    return database.drizzle
      .select({
        resource: resourceColumns,
        lineage: resourceLineageColumns,
        canvasPosition: canvasPositionColumns,
        projectSlug: project.slug,
        environmentSlug: environment.namespace,
      })
      .from(environmentResource)
      .innerJoin(environment, eq(environmentResource.environmentId, environment.id))
      .innerJoin(project, eq(environment.projectId, project.id))
      .innerJoin(resourceLineage, eq(environmentResource.lineageId, resourceLineage.id))
      .leftJoin(
        environmentCanvasNodePosition,
        and(
          eq(environmentCanvasNodePosition.resourceType, "volume"),
          eq(environmentCanvasNodePosition.resourceId, environmentResource.id),
        ),
      );
  },
);

export const getVolumeResource = Effect.fn(
  "EnvironmentDesign.getVolumeResource",
)(function* (environmentId: string, resourceId: string) {
  const query = yield* baseVolumeQuery();
  const rows = yield* query
    .where(
      and(
        eq(environmentResource.environmentId, environmentId),
        eq(environmentResource.id, resourceId),
        eq(environmentResource.implementationType, "volume"),
      ),
    )
    .limit(1);
  const records = yield* projectVolumeRows(rows);
  return records[0] ?? null;
});

export const createVolumeAggregate = Effect.fn(
  "EnvironmentDesign.createVolumeAggregate",
)(function* (input: {
  readonly projectId: string;
  readonly environmentId: string;
  readonly name: string;
  readonly slug: string;
  readonly x: number;
  readonly y: number;
}) {
  const database = yield* Database;
  const identity = yield* createResourceLineage(input);
  if (identity === null) return null;
  const resources = yield* database.drizzle
    .insert(environmentResource)
    .values({
      organizationId: organizationIdForProject(input.projectId),
      projectId: input.projectId,
      environmentId: input.environmentId,
      lineageId: identity.id,
      implementationType: "volume",
      variableGroupId: null,
      name: input.name,
      slug: input.slug,
    })
    .onConflictDoNothing()
    .returning({ id: environmentResource.id });
  const resource = resources[0];
  if (resource === undefined) {
    yield* deleteResourceLineage(identity.id);
    return null;
  }
  yield* database.drizzle.insert(environmentCanvasNodePosition).values({
    organizationId: organizationIdForEnvironment(input.environmentId),
    environmentId: input.environmentId,
    resourceType: "volume",
    resourceId: resource.id,
    x: Math.round(input.x),
    y: Math.round(input.y),
  });
  return resource.id;
});

export const updateVolumeName = Effect.fn("EnvironmentDesign.updateVolumeName")(
  function* (resourceId: string, name: string) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .update(environmentResource)
      .set({ name, updatedAt: new Date() })
      .where(eq(environmentResource.id, resourceId))
      .returning({ id: environmentResource.id });
    return rows[0] ?? null;
  },
);

export const tombstoneVolume = Effect.fn("EnvironmentDesign.tombstoneVolume")(
  function* (resourceId: string) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .update(environmentResource)
      .set({ deletedAt: new Date(), updatedAt: new Date() })
      .where(eq(environmentResource.id, resourceId))
      .returning({ resourceId: environmentResource.id });
    return rows[0] ?? null;
  },
);

export const upsertResourceCanvasPosition = Effect.fn(
  "EnvironmentDesign.upsertResourceCanvasPosition",
)(function* (input: {
  readonly environmentId: string;
  readonly resourceId: string;
  readonly resourceType: "variable_group" | "volume";
  readonly x: number;
  readonly y: number;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .insert(environmentCanvasNodePosition)
    .values({
      organizationId: organizationIdForEnvironment(input.environmentId),
      environmentId: input.environmentId,
      resourceType: input.resourceType,
      resourceId: input.resourceId,
      x: Math.round(input.x),
      y: Math.round(input.y),
      updatedAt: new Date(),
    })
    .onConflictDoUpdate({
      target: [
        environmentCanvasNodePosition.environmentId,
        environmentCanvasNodePosition.resourceType,
        environmentCanvasNodePosition.resourceId,
      ],
      set: {
        x: Math.round(input.x),
        y: Math.round(input.y),
        updatedAt: new Date(),
      },
    })
    .returning(canvasPositionColumns);
  return rows[0] ?? null;
});
