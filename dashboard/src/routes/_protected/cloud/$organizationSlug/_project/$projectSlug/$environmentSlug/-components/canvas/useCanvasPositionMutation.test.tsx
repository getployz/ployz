// @vitest-environment jsdom
import { createTransaction } from "@tanstack/react-db";
import { QueryClient } from "@tanstack/react-query";
import { act, cleanup, render, renderHook, screen, waitFor } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { expect, it, vi } from "vitest";
import { createApiCollection, preloadCollection } from "#/collections/query-collection";
import type { environmentCanvasNodePosition } from "#/modules/canvas/tables";
import { canvasPositionKey } from "#/modules/canvas/canvas-positions";
import * as scopes from "#/collections/use-collection-scope";
import * as collections from "#/collections/collections";
import { persistCanvasPositionBatch, useCanvasPositionMutation, usePlaceNewNode } from "./useCanvasPositionMutation";
import type { CanvasStoreConfigNode } from "./types";
import type { MouseEvent } from "react";
import { toast } from "sonner";
import * as positionFunctions from "#/modules/canvas/canvas-positions.functions";
import { asTestDouble } from "#/lib/test-double";

/** What a server function rejects with: an encoded PublicError, a plain object. */
const refusal = { _tag: "PublicError", code: "CONFLICT", message: "The Environment changed. Reload and try again." };

type PositionRow = typeof environmentCanvasNodePosition.$inferSelect;

async function positions(rows: PositionRow[]) {
  const queryClient = new QueryClient();
  const collection = createApiCollection({ queryClient, queryKey: ["canvas-positions"], queryFn: async () => rows,
    getKey: canvasPositionKey });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(collections, "getCanvasPositionsCollection").mockReturnValue(collection);
  // The save the hooks call through `useServerFn` is refused, as the server function's encoder sends it.
  vi.spyOn(positionFunctions, "updateCanvasPositionServerFn").mockRejectedValue(refusal);
  const toastError = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  await preloadCollection(collection);
  return { collection, toastError, done: async () => {
    cleanup();
    vi.restoreAllMocks();
    await collection.cleanup();
    queryClient.clear();
  } };
}

function organizationRouter(Page: () => React.ReactNode) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const cloud = createRoute({ getParentRoute: () => protectedRoute, path: "cloud", component: Outlet });
  const organization = createRoute({ getParentRoute: () => cloud, path: "$organizationSlug",
    loader: () => ({ organizationId: "org" }), component: Outlet });
  const project = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const newProject = createRoute({ getParentRoute: () => project, path: "new", component: Page });
  return createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([cloud.addChildren([
    organization.addChildren([project.addChildren([newProject])]),
  ])])]), history: createMemoryHistory({ initialEntries: ["/cloud/acme/new"] }) });
}

it("places new nodes on the new-project route without an Environment match", async () => {
  const queryClient = new QueryClient();
  const collection = createApiCollection({ queryClient, queryKey: ["new-project-positions"],
    queryFn: async () => [], getKey: (row: typeof environmentCanvasNodePosition.$inferSelect) => row.id });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(collections, "getCanvasPositionsCollection").mockReturnValue(collection);
  const router = organizationRouter(() => {
    usePlaceNewNode("acme");
    return <div>Choose a source</div>;
  });
  try {
    await router.load();
    render(<RouterProvider router={router} />);
    expect(await screen.findByText("Choose a source")).toBeTruthy();
  } finally {
    cleanup();
    router.history.destroy();
    vi.restoreAllMocks();
    await collection.cleanup();
    queryClient.clear();
  }
});

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
  let started = () => {};
  const inFlight = new Promise<void>((resolve) => { started = resolve; });
  const transaction = createTransaction({ autoCommit: false, mutationFn: () => (started(), persistCanvasPositionBatch([
    Promise.reject(writeFailure),
    sibling.then(() => {
      const original = saved.find((row) => row.resourceType === "volume");
      if (!original) throw new Error("Missing volume fixture");
      const data = { ...original, x: 20, y: 20 };
      saved = saved.map((row) => row.resourceType === "volume" ? data : row);
      return { data };
    }),
  ], collection)) });
  transaction.mutate(() => {
    collection.update("service:service", (row) => { row.x = 10; row.y = 10; });
    collection.update("volume:volume", (row) => { row.x = 20; row.y = 20; });
  });
  try {
    const outcome = transaction.commit().catch((error: Error) => error);
    await inFlight;
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

it("rolls a drag back and shows the server's sentence when its save is refused", async () => {
  const { collection, toastError, done } = await positions([{ id: "sentry", organizationId: "org", environmentId: "env", resourceType: "config",
    resourceId: "sentry", x: 0, y: 0, createdAt: new Date(0), updatedAt: new Date(0) }]);
  try {
    const { result } = renderHook(() => useCanvasPositionMutation({ organizationId: "org", organizationSlug: "acme",
      projectSlug: "shop", environmentSlug: "production" }));
    const node: CanvasStoreConfigNode = { id: "sentry", type: "storeConfig", position: { x: 40, y: 40 }, data: {
      config: { id: "sentry", name: "sentry", files: [], mounts: [], deployed: true, change: null },
      environmentId: "env", resourceType: "config", resourceId: "sentry" } };
    const drag = asTestDouble<MouseEvent>()({});
    act(() => {
      result.current.onNodeDrag(drag, node, [node]);
      result.current.onNodeDrag(drag, { ...node, position: { x: 80, y: 80 } }, [node]);
    });
    await waitFor(() => expect(toastError).toHaveBeenCalledWith(refusal.message));
    expect(toastError).toHaveBeenCalledTimes(1);
    expect(collection.get("config:sentry")).toMatchObject({ x: 0, y: 0 });
  } finally {
    await done();
  }
});

it("shows the server's sentence when a new node's place is refused", async () => {
  const { collection, toastError, done } = await positions([]);
  let place: ReturnType<typeof usePlaceNewNode> = () => {};
  const router = organizationRouter(() => {
    place = usePlaceNewNode("acme");
    return <div>Choose a source</div>;
  });
  try {
    await router.load();
    render(<RouterProvider router={router} />);
    await screen.findByText("Choose a source");
    act(() => place({ environmentId: "env", resourceType: "service", resourceId: "web", x: 40, y: 40 }));
    await waitFor(() => expect(toastError).toHaveBeenCalledWith(refusal.message));
    expect(collection.get("service:web")).toBeUndefined();
  } finally {
    router.history.destroy();
    await done();
  }
});
