import { parseResourceConfig } from "@ployz/sdk/config";
import type { environmentResource, resourceLineage } from "./tables";
import type { SavedEnvironmentIntent } from "./saved-intent";
import type { ServiceCanvasPositionRecord } from "./services";
import { variableDocumentRecord } from "./variable-document";
import { decodeStrict } from "./schema";
import { variableGroupResourceRecordSchema, volumeResourceRecordSchema } from "./resources";
import { slugifySegment } from "#/utils/slug";

export type ResourceDocumentView = {
  document: { intent: SavedEnvironmentIntent; projectId: string; updatedAt: Date };
  resource: Omit<typeof environmentResource.$inferSelect, "organizationId">;
  lineage: Omit<typeof resourceLineage.$inferSelect, "organizationId">;
  canvasPosition: ServiceCanvasPositionRecord | null;
  projectSlug: string;
  environmentSlug: string;
};

export function variableGroupDocumentRecord(row: ResourceDocumentView) {
  const { id: resourceId, environmentId } = row.resource;
  const { document, ...view } = row;
    const node = document.intent.variableGroups.find((node) => node.resourceId === resourceId);
    if (!node) return null;
    const variables = node.variables.map((variable) => variableDocumentRecord(variable, { serviceId: null, variableGroupId: node.variableGroupId }, document.intent, document.updatedAt));
    return decodeStrict(variableGroupResourceRecordSchema, { ...view,
      resource: { ...view.resource, name: node.name, slug: node.slug, deletedAt: null },
      variableGroup: { id: node.variableGroupId, lineageId: node.variableGroupLineageId, projectId: document.projectId, environmentId, name: node.name, slug: node.slug, createdAt: view.resource.createdAt, updatedAt: document.updatedAt },
      variables, exports: variables.filter((variable) => variable.exported).map((variable) => ({ key: variable.key, value: variable.value, variableId: variable.id })),
      consumerCount: document.intent.services.filter((service) => service.variableGroupAttachments.some((attachment) => attachment.variableGroupId === node.variableGroupId)).length,
    });
}

export type VolumeHistory = {
  snapshot: { config: unknown; createdAt: Date } | null;
  removedAt: Date | null;
};

export function volumeIsVisible(row: ResourceDocumentView, history: VolumeHistory) {
  return row.document.intent.volumes.some((node) => node.resourceId === row.resource.id)
    || history.removedAt === null
    || (history.snapshot?.createdAt ?? row.resource.createdAt) > history.removedAt;
}

export function volumeDocumentRecord(row: ResourceDocumentView, history: VolumeHistory) {
  const resourceId = row.resource.id;
    const { document, ...view } = row;
    const node = document.intent.volumes.find((node) => node.resourceId === resourceId);
    if (!volumeIsVisible(row, history)) return null;
    const name = node?.name ?? (history.snapshot ? parseResourceConfig("volume", history.snapshot.config).name : row.lineage.canonicalName);
    const attachments = document.intent.services.flatMap((service) => service.volumeAttachments.filter((mount) => mount.volumeResourceId === resourceId).map((mount) => ({ serviceId: service.id, mountPath: mount.mountPath })));
    return decodeStrict(volumeResourceRecordSchema, { ...view, resource: { ...view.resource, name, slug: slugifySegment(name) || "volume", deletedAt: node ? null : row.document.updatedAt }, attachments, consumerCount: attachments.length, runtimeStatus: null });
}
