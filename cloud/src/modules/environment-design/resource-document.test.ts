import { expect, it } from "vitest";
import { volumeDocumentRecord, type ResourceDocumentView } from "./resource-document";

it("keeps pending volume removal visible without making history editable", () => {
  const id = "00000000-0000-4000-8000-000000000001";
  const lineageId = "00000000-0000-4000-8000-000000000002";
  const environmentId = "00000000-0000-4000-8000-000000000003";
  const projectId = "00000000-0000-4000-8000-000000000004";
  const before = new Date("2026-09-01T00:00:00Z");
  const removed = new Date("2026-09-02T00:00:00Z");
  const after = new Date("2026-09-03T00:00:00Z");
  const row: ResourceDocumentView = {
    document: { projectId, updatedAt: after, intent: { version: 1, environmentSlug: "production", services: [], variableGroups: [], volumes: [] } },
    resource: { id, projectId, environmentId, lineageId, implementationType: "volume", variableGroupId: null, createdAt: before, updatedAt: before },
    lineage: { id: lineageId, projectId, canonicalName: "Original name", canonicalSlug: "original-name", createdAt: before, updatedAt: before },
    canvasPosition: null, projectSlug: "test", environmentSlug: "production",
  };
  const snapshot = { config: { version: 2, name: "Last deployed name" }, createdAt: before };
  expect(volumeDocumentRecord(row, { snapshot, removedAt: null })?.resource).toMatchObject({ name: "Last deployed name", deletedAt: after });
  expect(volumeDocumentRecord(row, { snapshot, removedAt: removed })).toBeNull();
  expect(volumeDocumentRecord(row, { snapshot: { ...snapshot, createdAt: after }, removedAt: removed })).not.toBeNull();
  row.document.intent.volumes.push({ resourceId: id, resourceLineageId: lineageId, name: "Restored name" });
  expect(volumeDocumentRecord(row, { snapshot, removedAt: removed })?.resource).toMatchObject({ name: "Restored name", deletedAt: null });
});
