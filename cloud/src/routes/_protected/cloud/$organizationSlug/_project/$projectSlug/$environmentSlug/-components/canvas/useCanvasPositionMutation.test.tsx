// @vitest-environment jsdom
import { createTransaction } from "@tanstack/react-db";
import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { createApiCollection, preloadCollection } from "#/collections/query-collection";
import type { environmentCanvasNodePosition } from "#/modules/environment-design/tables";
import { persistCanvasPositionBatch } from "./useCanvasPositionMutation";

it.each([false, true])("applies committed sibling writes and preserves the batch failure (refresh fails: %s)", async (refreshFails) => {
  const queryClient = new QueryClient();
  let saved: (typeof environmentCanvasNodePosition.$inferSelect)[] = ["service", "volume"].map((resourceType) => ({
    id: resourceType, resourceType, resourceId: resourceType, organizationId: "org", environmentId: "env",
    x: 0, y: 0, createdAt: new Date(0), updatedAt: new Date(0),
  }));
  const read = vi.fn(async () => saved);
  const collection = createApiCollection({ queryClient, queryKey: ["canvas-batch"], queryFn: read,
    getKey: (row: (typeof saved)[number]) => `${row.resourceType}:${row.resourceId}` });
  await preloadCollection(collection);
  const writeFailure = new Error("service write rejected");
  let finishSibling = () => {};
  const sibling = new Promise<void>((resolve) => { finishSibling = resolve; });
  const transaction = createTransaction({ autoCommit: false, mutationFn: () => persistCanvasPositionBatch([
    Promise.reject(writeFailure),
    sibling.then(() => {
      const original = saved.find((row) => row.resourceType === "volume");
      if (!original) throw new Error("Missing volume fixture");
      const data = { ...original, x: 20, y: 20 };
      saved = saved.map((row) => row.resourceType === "volume" ? data : row);
      return { data };
    }),
  ], collection) });
  transaction.mutate(() => {
    collection.update("service:service", (row) => { row.x = 10; row.y = 10; });
    collection.update("volume:volume", (row) => { row.x = 20; row.y = 20; });
  });
  try {
    const outcome = transaction.commit().catch((error: Error) => error);
    // Let the first rejection settle while its sibling is still in flight.
    await new Promise((resolve) => setTimeout(resolve, 0));
    const readsBeforeSibling = read.mock.calls.length;
    if (refreshFails) read.mockRejectedValue(new Error("refresh failed"));
    finishSibling();
    expect(await outcome).toBe(writeFailure);
    expect(read.mock.calls.length).toBeLessThanOrEqual(readsBeforeSibling + 1);
    expect(collection.get("service:service")).toMatchObject({ x: 0, y: 0 });
    expect(collection.get("volume:volume")).toMatchObject({ x: 20, y: 20 });
  } finally {
    finishSibling();
    await collection.cleanup();
    queryClient.clear();
  }
});
