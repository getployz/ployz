// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { focusManager, onlineManager, QueryClient } from "@tanstack/react-query";
import { createApiCollection, preloadCollection, reconcileCollection } from "./query-collection";

afterEach(() => {
  vi.useRealTimers();
  focusManager.setFocused(undefined);
  onlineManager.setOnline(true);
});

it("refreshes active snapshots, retains failed reads, and stops unused scopes", async () => {
  vi.useFakeTimers();
  focusManager.setFocused(true);
  const client = new QueryClient();
  client.mount();
  let rows = [{ id: "a", name: "before" }];
  const read = vi.fn(async () => rows);
  const collection = createApiCollection({
    queryClient: client, queryKey: ["test", "session"], queryFn: read,
    getKey: (row: { id: string; name: string }) => row.id,
  });
  const subscription = collection.subscribeChanges(() => {});
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.get("a")?.name).toBe("before");
  rows = [{ id: "b", name: "after" }];
  await vi.advanceTimersByTimeAsync(15_000);
  expect(collection.get("a")).toBeUndefined();
  expect(collection.get("b")?.name).toBe("after");
  read.mockRejectedValueOnce(new Error("unavailable"));
  await vi.advanceTimersByTimeAsync(15_000);
  expect(collection.get("b")?.name).toBe("after");
  expect(collection.utils.isError).toBe(true);
  focusManager.setFocused(false);
  const hiddenReads = read.mock.calls.length;
  rows = [{ id: "c", name: "focused" }];
  await vi.advanceTimersByTimeAsync(30_000);
  expect(read).toHaveBeenCalledTimes(hiddenReads);
  focusManager.setFocused(true);
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.get("c")?.name).toBe("focused");
  onlineManager.setOnline(false);
  rows = [];
  onlineManager.setOnline(true);
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.size).toBe(0);
  subscription.unsubscribe();
  await vi.advanceTimersByTimeAsync(2_000);
  const inactiveReads = read.mock.calls.length;
  await vi.advanceTimersByTimeAsync(30_000);
  expect(read).toHaveBeenCalledTimes(inactiveReads);
  await collection.cleanup();
  client.unmount();
  client.clear();
});

it("rejects failed initial loader readiness, retries later, and retains successful snapshots on refresh failure", async () => {
  const client = new QueryClient();
  const failure = new Error("API unavailable");
  const read = vi.fn<() => Promise<{ id: string }[]>>().mockRejectedValue(failure);
  const collection = createApiCollection({ queryClient: client, queryKey: ["failed-preload"], queryFn: read,
    getKey: (row: { id: string }) => row.id });
  await expect(preloadCollection(collection)).rejects.toBe(failure);
  expect(collection.subscriberCount).toBe(0);
  read.mockResolvedValue([{ id: "retained" }]);
  await preloadCollection(collection);
  expect(collection.get("retained")).toMatchObject({ id: "retained" });
  const active = collection.subscribeChanges(() => {});
  read.mockRejectedValue(failure);
  await expect(collection.utils.refetch({ throwOnError: true })).rejects.toBe(failure);
  await expect(preloadCollection(collection)).resolves.toBeUndefined();
  expect(collection.get("retained")).toMatchObject({ id: "retained" });
  active.unsubscribe();
  await collection.cleanup();
  client.clear();
});

it("releases preload-only and reconciliation observers while keeping mounted consumers active", async () => {
  vi.useFakeTimers();
  focusManager.setFocused(true);
  const client = new QueryClient();
  client.mount();
  const read = vi.fn(async () => [{ id: "loaded" }]);
  const collection = createApiCollection({ queryClient: client, queryKey: ["preload-only"], queryFn: read,
    getKey: (row: { id: string }) => row.id });
  await preloadCollection(collection);
  expect(collection.subscriberCount).toBe(0);
  const preloadedReads = read.mock.calls.length;
  await vi.advanceTimersByTimeAsync(30_000);
  expect(read).toHaveBeenCalledTimes(preloadedReads);
  // An abandoned prefetch keeps its snapshot ready without running an active observer.
  expect(collection.get("loaded")).toMatchObject({ id: "loaded" });
  await reconcileCollection(collection);
  expect(read.mock.calls.length).toBeGreaterThan(preloadedReads);
  const reconciledReads = read.mock.calls.length;
  await vi.advanceTimersByTimeAsync(30_000);
  expect(read).toHaveBeenCalledTimes(reconciledReads);
  const active = collection.subscribeChanges(() => {});
  await vi.advanceTimersByTimeAsync(10);
  await preloadCollection(collection);
  await reconcileCollection(collection);
  const activeReads = read.mock.calls.length;
  await vi.advanceTimersByTimeAsync(15_000);
  expect(read.mock.calls.length).toBeGreaterThan(activeReads);
  active.unsubscribe();
  const unusedReads = read.mock.calls.length;
  await vi.advanceTimersByTimeAsync(30_000);
  expect(read).toHaveBeenCalledTimes(unusedReads);
  await collection.cleanup();
  client.unmount();
  client.clear();
});

it.each([false, true])("applies committed rows without an observer and cancels older reads (preloaded: %s)", async (preloaded) => {
  const client = new QueryClient();
  const stale = { id: "row", revision: "old" };
  const committed = { id: "row", revision: "committed" };
  const read = vi.fn<() => Promise<typeof stale[]>>().mockResolvedValue([stale]);
  const collection = createApiCollection({ queryClient: client, queryKey: ["committed", String(preloaded)],
    queryFn: read, getKey: (row: typeof stale) => row.id });
  if (preloaded) await preloadCollection(collection);
  let finishRead = (_rows: typeof stale[]) => {};
  read.mockImplementation(() => new Promise((resolve) => { finishRead = resolve; }));
  const active = collection.subscribeChanges(() => {});
  const pendingRead = collection.utils.refetch();
  await collection.writeCommitted(committed);
  finishRead([stale]);
  await pendingRead;
  expect(collection.get("row")).toMatchObject(committed);
  active.unsubscribe();
  read.mockRejectedValue(new Error("GET unavailable"));
  await reconcileCollection(collection);
  expect(collection.get("row")).toMatchObject(committed);
  expect(collection.utils.isError).toBe(true);
  expect(collection.subscriberCount).toBe(0);
  await collection.cleanup();
  client.clear();
});
