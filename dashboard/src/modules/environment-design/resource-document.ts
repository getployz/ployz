import { parseResourceConfig } from "@ployz/sdk/config";
import type { environmentResource, resourceLineage } from "./tables";
import type { SavedEnvironmentIntent } from "./saved-intent";
import type { ServiceCanvasPositionRecord } from "./services";
import { decodeStrict } from "./schema";
import { volumeResourceRecordSchema } from "./resources";
import { slugifySegment } from "#/utils/slug";

export type ResourceDocumentView = {
  document: { intent: SavedEnvironmentIntent; projectId: string; updatedAt: Date };
  resource: Omit<typeof environmentResource.$inferSelect, "organizationId">;
  lineage: Omit<typeof resourceLineage.$inferSelect, "organizationId">;
  canvasPosition: ServiceCanvasPositionRecord | null;
  projectSlug: string;
  environmentSlug: string;
};

export type VolumeHistory = {
  snapshot: { config: unknown; createdAt: Date } | null;
  removedAt: Date | null;
};

export function volumeIsVisible(row: ResourceDocumentView, history: VolumeHistory) {
  return row.document.intent.volumes.some((node) => node.resourceId === row.resource.id)
    || (history.snapshot !== null
      && (history.removedAt === null || history.snapshot.createdAt > history.removedAt));
}

export function volumeDocumentRecord(row: ResourceDocumentView, history: VolumeHistory) {
  const resourceId = row.resource.id;
    const { document, ...view } = row;
    const node = document.intent.volumes.find((node) => node.resourceId === resourceId);
    if (!volumeIsVisible(row, history)) return null;
    const name = node?.name ?? (history.snapshot ? parseResourceConfig("volume", history.snapshot.config).name : row.lineage.canonicalName);
    const attachments = document.intent.services.flatMap((service) => service.volumeAttachments.filter((mount) => mount.volumeResourceId === resourceId).map((mount) => ({ serviceId: service.id, mountPath: mount.mountPath })));
    return decodeStrict(volumeResourceRecordSchema, { ...view, resource: { ...view.resource, name, slug: slugifySegment(name) || "volume", deletedAt: node ? null : row.document.updatedAt }, attachments, consumerCount: attachments.length, isAuthored: node !== undefined, runtimeStatus: null });
}
