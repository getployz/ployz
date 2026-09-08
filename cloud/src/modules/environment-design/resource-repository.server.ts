import { environmentNodeConfigSnapshot, volumeRemoveAttempt } from "#/modules/runtime/tables";
import { destructiveVolumeAttempt } from "#/modules/operations/tables";
import { variableGroupDocumentRecord, volumeDocumentRecord } from "./resource-document";
import "@tanstack/react-start/server-only";
import { and, desc, eq, isNotNull } from "drizzle-orm";
import { Effect } from "effect";
import { environmentCanvasNodePosition, environmentResource, environmentVariableGroup, resourceLineage } from "./tables";
import { project } from "#/modules/project/tables";
import { organizationIdForEnvironment, organizationIdForProject } from "#/db/scope-values.server";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { createResourceLineage, createVariableGroupLineage } from "./authoring-repository.server";
import { loadEnvironmentDocument } from "./working-state-repository.server";

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


const loadResourceRecord = Effect.fn("EnvironmentDesign.loadResourceRecord")(
  function* (environmentId: string, resourceId: string) {
    const { drizzle } = yield* Database;
    const document = yield* loadEnvironmentDocument(environmentId);
    const [row] = yield* drizzle.select({ resource: environmentResource, lineage: resourceLineage, canvasPosition: canvasPositionColumns, projectSlug: project.slug })
      .from(environmentResource).innerJoin(resourceLineage, eq(resourceLineage.id, environmentResource.lineageId))
      .innerJoin(project, eq(project.id, environmentResource.projectId))
      .leftJoin(environmentCanvasNodePosition, and(eq(environmentCanvasNodePosition.environmentId, environmentId), eq(environmentCanvasNodePosition.resourceId, resourceId), eq(environmentCanvasNodePosition.resourceType, environmentResource.implementationType)))
      .where(and(eq(environmentResource.environmentId, environmentId), eq(environmentResource.id, resourceId)));
    if (!row) return null;
    const { organizationId: _organization, ...resource } = row.resource;
    const { organizationId: _lineageOrganization, ...lineage } = row.lineage;
    return { document, resource, lineage, canvasPosition: row.canvasPosition, projectSlug: row.projectSlug, environmentSlug: document.namespace };
  },
);

export const getVariableGroupResource = Effect.fn("EnvironmentDesign.getVariableGroupResource")(
  function* (environmentId: string, resourceId: string) {
    const row = yield* loadResourceRecord(environmentId, resourceId);
    if (!row) return null;
    return variableGroupDocumentRecord(row);
  },
);

export const getVolumeResource = Effect.fn("EnvironmentDesign.getVolumeResource")(
  function* (environmentId: string, resourceId: string) {
    const row = yield* loadResourceRecord(environmentId, resourceId);
    if (!row || row.resource.implementationType !== "volume") return null;
    const { drizzle } = yield* Database;
    const [snapshots, direct, destructive] = yield* Effect.all([
      drizzle.select({ config: environmentNodeConfigSnapshot.config, createdAt: environmentNodeConfigSnapshot.createdAt })
        .from(environmentNodeConfigSnapshot).where(and(eq(environmentNodeConfigSnapshot.environmentId, environmentId), eq(environmentNodeConfigSnapshot.nodeType, "volume"), eq(environmentNodeConfigSnapshot.nodeId, resourceId), isNotNull(environmentNodeConfigSnapshot.config)))
        .orderBy(desc(environmentNodeConfigSnapshot.createdAt)).limit(1),
      drizzle.select({ terminalAt: volumeRemoveAttempt.terminalAt }).from(volumeRemoveAttempt)
        .where(and(eq(volumeRemoveAttempt.environmentId, environmentId), eq(volumeRemoveAttempt.environmentResourceId, resourceId), eq(volumeRemoveAttempt.status, "completed")))
        .orderBy(desc(volumeRemoveAttempt.terminalAt)).limit(1),
      drizzle.select({ terminalAt: destructiveVolumeAttempt.terminalAt }).from(destructiveVolumeAttempt)
        .where(and(eq(destructiveVolumeAttempt.environmentResourceId, resourceId), eq(destructiveVolumeAttempt.disposition, "completed")))
        .orderBy(desc(destructiveVolumeAttempt.terminalAt)).limit(1),
    ]);
    const dates = [...direct, ...destructive].flatMap((row) => row.terminalAt ? [row.terminalAt.getTime()] : []);
    return volumeDocumentRecord(row, { snapshot: snapshots[0] ?? null, removedAt: dates.length ? new Date(Math.max(...dates)) : null });
  },
);

export const getResourceIdentity = Effect.fn("EnvironmentDesign.getResourceIdentity")(
  function* (environmentId: string, resourceId: string) {
    const { intent } = yield* loadEnvironmentDocument(environmentId);
    if (intent.variableGroups.some((node) => node.resourceId === resourceId)) return { id: resourceId, implementationType: "variable_group" as const };
    if (intent.volumes.some((node) => node.resourceId === resourceId)) return { id: resourceId, implementationType: "volume" as const };
    return null;
  },
);

export const createResourceIdentity = Effect.fn("EnvironmentDesign.createResourceIdentity")(
  function* (input: { projectId: string; environmentId: string; name: string; slug: string; type: "volume" | "variable_group"; x: number; y: number }) {
    const { drizzle } = yield* Database;
    const lineage = yield* createResourceLineage(input);
    if (!lineage) return yield* new Conflict({ message: "Could not allocate resource lineage." });
    let group = null;
    if (input.type === "variable_group") {
      const groupLineage = yield* createVariableGroupLineage(input);
      if (!groupLineage) return yield* new Conflict({ message: "Could not allocate variable group lineage." });
      const [created] = yield* drizzle.insert(environmentVariableGroup).values({ organizationId: organizationIdForProject(input.projectId), projectId: input.projectId, environmentId: input.environmentId, lineageId: groupLineage.id }).returning();
      if (!created) return yield* Effect.die("PostgreSQL did not return variable group identity.");
      group = created;
    }
    const [resource] = yield* drizzle.insert(environmentResource).values({ organizationId: organizationIdForProject(input.projectId), projectId: input.projectId, environmentId: input.environmentId, lineageId: lineage.id, implementationType: input.type, variableGroupId: group?.id ?? null }).returning();
    if (!resource) return yield* Effect.die("PostgreSQL did not return resource identity.");
    yield* upsertResourceCanvasPosition({ ...input, resourceId: resource.id, resourceType: input.type });
    return { resource, group };
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
