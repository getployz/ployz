import { getDbClient } from "#/collections/scope";
import { expect, it, vi } from "vitest";
import { createOptimisticAction } from "@tanstack/react-db";
import { QueryClient } from "@tanstack/react-query";
import { createApiCollection, reconcileCollection } from "#/collections/query-collection";
import { createVolumeResourcesCollection } from "./resource.collection";
import { compileSavedEnvironmentIntent, type SavedEnvironmentIntent } from "./saved-intent";
import type { Collection } from "@tanstack/react-db";
type Sources = Parameters<typeof createVolumeResourcesCollection>[0]["sources"];
type Row<Key extends keyof Sources> = Sources[Key] extends Collection<infer R, infer _K, infer _U, infer _S, infer _I> ? R : never;

function createStores() {
  const client = new QueryClient();
  const collection = <T extends { id: string }>(rows: () => T[]) => createApiCollection({
    queryClient: client, queryKey: ["resources", crypto.randomUUID()], queryFn: async () => structuredClone(rows()), getKey: (row: T) => row.id,
  });
  const environmentId = "00000000-0000-4000-8000-000000000001";
  const projectId = "00000000-0000-4000-8000-000000000002";
  const organizationId = "00000000-0000-4000-8000-000000000003";
  const id = "00000000-0000-4000-8000-000000000004";
  const lineageId = "00000000-0000-4000-8000-000000000005";
  const now = new Date("2026-09-08T00:00:00Z");
  const intent: SavedEnvironmentIntent = { version: 1, environmentSlug: "production", services: [], volumes: [{ resourceId: id, resourceLineageId: lineageId, name: "data" }] };
  const server = {
    resources: [{ id, environmentId, projectId, organizationId, lineageId, implementationType: "volume", deployedName: null, removedAt: null, createdAt: now, updatedAt: now }] satisfies Row<"resources">[],
    lineages: [{ id: lineageId, projectId, organizationId, canonicalName: "data", canonicalSlug: "data", createdAt: now, updatedAt: now }] satisfies Row<"lineages">[],
    documents: [{ id: environmentId, projectId, organizationId, projectSlug: "app", namespace: "production", name: "Production", revision: "00000000-0000-4000-8000-000000000099", createdAt: now, updatedAt: now, intent, compiled: compileSavedEnvironmentIntent({ environmentId, intent }) }] satisfies Row<"documents">[],
    positions: [{ id: "00000000-0000-4000-8000-000000000006", environmentId, organizationId, resourceType: "volume", resourceId: id, x: 10, y: 20, createdAt: now, updatedAt: now }] satisfies Row<"positions">[],
  };
  const sources = {
    resources: collection<Row<"resources">>(() => server.resources),
    lineages: collection<Row<"lineages">>(() => server.lineages),
    documents: collection<Row<"documents">>(() => server.documents),
    positions: collection<Row<"positions">>(() => server.positions),
  } satisfies Sources;
  return { sources, id, environmentId, client, server };
}

it("hides a never-deployed volume when only Working State removes it", async () => {
  const { sources, client, server, id } = createStores();
  await Promise.all(Object.values(sources).map(reconcileCollection));
  const volumes = createVolumeResourcesCollection({ client: getDbClient(client), sources });
  try {
    await volumes.preload();
    expect(volumes.get(id)).toBeDefined();
    const document = server.documents[0];
    if (!document) throw new Error("Missing document fixture");
    server.documents = [{ ...document, intent: { ...document.intent, volumes: [] } }];
    await reconcileCollection(sources.documents);
    await vi.waitFor(() => expect(volumes.get(id)).toBeUndefined());
    expect(sources.resources.get(id)).toBeDefined();
  } finally {
    await volumes.cleanup();
    await Promise.all(Object.values(sources).map((collection) => collection.cleanup()));
    client.clear();
  }
});

it("refreshes joined resources after API creation, optimistic position persistence and deletion", async () => {
  const stores = createStores();
  await Promise.all(Object.values(stores.sources).map(reconcileCollection));
  const volumes = createVolumeResourcesCollection({ client: getDbClient(stores.client), sources: stores.sources });
  await volumes.preload();
  expect(volumes.get(stores.id)).toMatchObject({ resource: { name: "data" }, isAuthored: true, canvasPosition: { x: 10, y: 20 } });
  expect(volumes.get(stores.id)?.canvasPosition).not.toHaveProperty("organizationId");
  const positions = stores.sources.positions;
  const position = [...positions.values()][0];
  if (!position) throw new Error("Position fixture is missing.");
  const save = createOptimisticAction<number>({
    onMutate: (x) => { positions.update(position.id, (draft) => { draft.x = x; }); },
    mutationFn: async (x) => {
      stores.server.positions = stores.server.positions.map((row) => ({ ...row, x }));
      await reconcileCollection(positions);
    },
  });
  const mutation = save(50);
  expect(positions.get(position.id)?.x).toBe(50);
  await mutation.isPersisted.promise;
  await vi.waitFor(() => expect(volumes.get(stores.id)?.canvasPosition?.x).toBe(50));
  const resources = stores.server.resources;
  stores.server.resources = [];
  await reconcileCollection(stores.sources.resources);
  await vi.waitFor(() => expect(volumes.size).toBe(0));
  stores.server.resources = resources;
  await reconcileCollection(stores.sources.resources);
  await vi.waitFor(() => expect(volumes.get(stores.id)?.canvasPosition?.x).toBe(50));
  await volumes.cleanup();
  await Promise.all(Object.values(stores.sources).map((collection) => collection.cleanup()));
  stores.client.clear();
});
