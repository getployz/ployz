// @vitest-environment node
import { expect, it, vi } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import { createChangeCollection, endRequestCollections, preloadCollection } from "./query-collection";
import type { CollectionRead } from "./read.contract";

it.each([false, true])("reads nothing more once a request's QueryClient is cleared under a page that still reads (ended first: %s)", async (ended) => {
  const client = new QueryClient();
  const read = vi.fn(async (): Promise<CollectionRead<{ id: string }>> => ({ full: true, rows: [{ id: "a" }], cursor: "1" }));
  const collection = createChangeCollection({ queryClient: client, queryKey: ["collections", "s", "u", "acme", "environment_canvas_node_position"],
    getKey: (row) => row.id, read });
  // A page's live query keeps reading the collection after SSR renders it.
  const page = collection.subscribeChanges(() => {});
  await preloadCollection(collection);
  expect(read).toHaveBeenCalledTimes(1);
  // SSR cleanup: the request's collections end, then the Query integration clears its QueryClient.
  if (ended) endRequestCollections(client);
  client.clear();
  // Without ending first, the collection asks for its Query again at once, and reads it outside the request.
  expect(client.getQueryCache().getAll()).toHaveLength(ended ? 0 : 1);
  if (!ended) await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(2));
  else expect(read).toHaveBeenCalledTimes(1);
  page.unsubscribe();
  await collection.cleanup({ dependents: true });
});
