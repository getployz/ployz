import { expect, it, vi } from "vitest";
import { createOptimisticAction } from "@tanstack/react-db";
import { QueryClient } from "@tanstack/react-query";
import { createApiCollection, reconcileCollection, preloadCollection } from "#/collections/query-collection";
import { createVolumeResourcesCollection } from "./resource-collections";
import { compileEnvironmentIntent, type SavedEnvironmentIntent } from "@ployz/sdk/config";
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
  const intent: SavedEnvironmentIntent = { version: 1, environmentSlug: "production", services: [], variableGroups: [], volumes: [{ resourceId: id, resourceLineageId: lineageId, name: "data" }] };
  const server = {
    resources: [{ id, environmentId, projectId, organizationId, lineageId, implementationType: "volume", variableGroupId: null, createdAt: now, updatedAt: now }] satisfies Row<"resources">[],
    lineages: [{ id: lineageId, projectId, organizationId, canonicalName: "data", canonicalSlug: "data", createdAt: now, updatedAt: now }] satisfies Row<"lineages">[],
    documents: [{ id: environmentId, projectId, organizationId, projectSlug: "app", namespace: "production", name: "Production", revision: "00000000-0000-4000-8000-000000000099", createdAt: now, updatedAt: now, intent, compiled: compileEnvironmentIntent(environmentId, intent) }] satisfies Row<"documents">[],
    positions: [{ id: "00000000-0000-4000-8000-000000000006", environmentId, organizationId, resourceType: "volume", resourceId: id, x: 10, y: 20, createdAt: now, updatedAt: now }] satisfies Row<"positions">[],
    snapshots: new Array<Row<"snapshots">>(),
    removals: new Array<Row<"removals">>(),
  };
  const sources = {
    resources: collection<Row<"resources">>(() => server.resources),
    lineages: collection<Row<"lineages">>(() => server.lineages),
    documents: collection<Row<"documents">>(() => server.documents),
    positions: collection<Row<"positions">>(() => server.positions),
    snapshots: collection<Row<"snapshots">>(() => server.snapshots),
    removals: collection<Row<"removals">>(() => server.removals),
  } satisfies Sources;
  return { sources, id, environmentId, client, server };
}

it("refreshes joined resources after API creation, optimistic position persistence and deletion", async () => {
  const stores = createStores();
  await Promise.all(Object.values(stores.sources).map(reconcileCollection));
  const volumes = createVolumeResourcesCollection({ organizationSlug: "test", sources: stores.sources });
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

it("updates joined volume history from API snapshots and completed removals", async () => {
  const { sources, id, environmentId, client, server } = createStores();
  await Promise.all(Object.values(sources).map(preloadCollection));
  const resource = sources.resources.get(id);
  if (!resource) throw new Error("Missing fixture resource");
  const now = resource.createdAt;
  let snapshots: Row<"snapshots">[] = [{ id: "snapshot", organizationId: resource.organizationId,
    environmentId, environmentDeploymentId: "deployment", nodeType: "volume", nodeId: id,
    nodeLineageId: resource.lineageId, configVersion: 1, config: { version: 2, name: "deployed-data" }, createdAt: now, updatedAt: now }];
  let removals: Row<"removals">[] = [];
  const snapshotCollection = createApiCollection({ queryClient: client, queryKey: ["history", "snapshots"],
    queryFn: async () => snapshots, getKey: (row: Row<"snapshots">) => row.id });
  const removalCollection = createApiCollection({ queryClient: client, queryKey: ["history", "removals"],
    queryFn: async () => removals, getKey: (row: Row<"removals">) => row.id });
  await Promise.all([preloadCollection(snapshotCollection), preloadCollection(removalCollection)]);
  const document = server.documents[0];
  if (!document) throw new Error("Missing fixture document");
  document.intent.volumes = [];
  await reconcileCollection(sources.documents);
  const volumes = createVolumeResourcesCollection({ organizationSlug: "history-test",
    sources: { ...sources, snapshots: snapshotCollection, removals: removalCollection } });
  try {
    await volumes.preload();
    expect(volumes.get(id)).toMatchObject({ isAuthored: false, resource: { name: "deployed-data" } });
    removals = [{ id: "removal", organizationId: resource.organizationId, requestedByUserId: "user", environmentId,
      environmentDeploymentId: "deployment", environmentResourceId: id, retryOfAttemptId: null, volumes: [],
      status: "completed", inngestRunId: null, outcome: null, failureMessage: null, startedAt: now,
      terminalAt: new Date(now.getTime() + 1), createdAt: now, updatedAt: now }];
    await removalCollection.utils.refetch({ throwOnError: true });
    await vi.waitFor(() => expect(volumes.get(id)).toBeUndefined());
    const previous = snapshots[0];
    if (!previous) throw new Error("Missing fixture snapshot");
    snapshots = [{ ...previous, id: "new-snapshot", config: { version: 2, name: "redeployed-data" }, createdAt: new Date(now.getTime() + 2) }];
    await snapshotCollection.utils.refetch({ throwOnError: true });
    await vi.waitFor(() => expect(volumes.get(id)?.resource.name).toBe("redeployed-data"));
  } finally {
    await volumes.cleanup();
    await snapshotCollection.cleanup();
    await removalCollection.cleanup();
    await Promise.all(Object.values(sources).map((collection) => collection.cleanup()));
    client.clear();
  }
});
