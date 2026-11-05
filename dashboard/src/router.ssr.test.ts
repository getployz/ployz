// @vitest-environment node
import { afterEach, expect, it, vi } from "vitest";
import { attachRouterServerSsrUtils } from "@tanstack/react-router/ssr/server";
import { createChangeCollection, preloadCollection } from "#/collections/query-collection";
import type { CollectionRead } from "#/collections/read.contract";
import { getRouter } from "./router";

afterEach(() => {
  vi.useRealTimers();
});

it("ends a request's collections before the Query integration clears its QueryClient", async () => {
  vi.useFakeTimers();
  const router = getRouter();
  const { queryClient } = router.options.context;
  attachRouterServerSsrUtils({ router, manifest: undefined });
  const read = vi.fn(async (): Promise<CollectionRead<{ id: string }>> => ({ full: true, rows: [{ id: "a" }], cursor: "1" }));
  const collection = createChangeCollection({ queryClient, queryKey: ["collections", "s", "u", "acme", "environment_canvas_node_position"],
    getKey: (row) => row.id, read });
  const page = collection.subscribeChanges(() => {});
  await preloadCollection(collection);
  // A HEAD request's dropped body ends SSR before render finishes.
  router.serverSsr?.cleanup();
  await vi.runAllTimersAsync();
  expect(read).toHaveBeenCalledTimes(1);
  page.unsubscribe();
});
