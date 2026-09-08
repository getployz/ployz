import { expect, it, vi } from "vitest";
import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { createVolumeResourcesCollection } from "./resource-collections";
import { compileEnvironmentIntent, type SavedEnvironmentIntent } from "@ployz/sdk/config";
import type { Collection } from "@tanstack/react-db";
type Sources = Parameters<typeof createVolumeResourcesCollection>[0]["sources"];
type Row<Key extends keyof Sources> = Sources[Key] extends Collection<infer R, infer _K, infer _U, infer _S, infer _I> ? R : never;

const stores = (() => {
  const collection = <T extends { id: string }>(initialData: T[]) => createCollection(localOnlyCollectionOptions<T>({ getKey: (row) => row.id, initialData }));
  const environmentId = "00000000-0000-4000-8000-000000000001";
  const projectId = "00000000-0000-4000-8000-000000000002";
  const organizationId = "00000000-0000-4000-8000-000000000003";
  const id = "00000000-0000-4000-8000-000000000004";
  const lineageId = "00000000-0000-4000-8000-000000000005";
  const now = new Date("2026-09-08T00:00:00Z");
  const intent: SavedEnvironmentIntent = { version: 1, environmentSlug: "production", services: [], variableGroups: [], volumes: [{ resourceId: id, resourceLineageId: lineageId, name: "data" }] };
  const sources: Sources = {
    resources: collection<Row<"resources">>([{ id, environmentId, projectId, organizationId, lineageId, implementationType: "volume", variableGroupId: null, createdAt: now, updatedAt: now }]),
    lineages: collection<Row<"lineages">>([{ id: lineageId, projectId, organizationId, canonicalName: "data", canonicalSlug: "data", createdAt: now, updatedAt: now }]),
    documents: collection<Row<"documents">>([{ id: environmentId, projectId, organizationId, projectSlug: "app", namespace: "production", name: "Production", revision: "00000000-0000-4000-8000-000000000099", createdAt: now, updatedAt: now, intent, compiled: compileEnvironmentIntent(environmentId, intent) }]),
    positions: collection<Row<"positions">>([{ id: "00000000-0000-4000-8000-000000000006", environmentId, organizationId, resourceType: "volume", resourceId: id, x: 10, y: 20, createdAt: now, updatedAt: now }]),
    snapshots: collection<Row<"snapshots">>([]),
    removals: collection<Row<"removals">>([]),
    destructive: collection<Row<"destructive">>([]),
  };
  return { sources, id, environmentId };
})();

it("loads an authored volume with its Electric canvas position and no runtime history", async () => {
  const volumes = createVolumeResourcesCollection({ organizationSlug: "test", sources: stores.sources });
  await volumes.preload();
  expect(volumes.get(stores.id)).toMatchObject({ resource: { name: "data", deletedAt: null }, canvasPosition: { x: 10, y: 20 } });
  expect(volumes.get(stores.id)?.canvasPosition).not.toHaveProperty("organizationId");
  stores.sources.documents.update(stores.environmentId, (document) => {
    const volume = document.intent.volumes.find((volume) => volume.resourceId === stores.id);
    if (volume) volume.name = "renamed-data";
  });
  await vi.waitFor(() => expect(volumes.get(stores.id)?.resource.name).toBe("renamed-data"));
  await volumes.cleanup();
});
