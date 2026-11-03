// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { focusManager, onlineManager, QueryClient } from "@tanstack/react-query";
import { createApiCollection, createChangeCollection, preloadCollection, reconcileCollection } from "./query-collection";
import type { CollectionRead } from "./read.contract";
import { getDbClient } from "./scope";

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
    queryClient: client, queryKey: ["test", "session"], queryFn: read, refetchInterval: 15_000,
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
  expect(collection.health.hasErrors).toBe(true);
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
  expect(read).toHaveBeenCalledTimes(2);
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

it.each(["api", "change"] as const)("leaves a failed first load when a later read succeeds, with one reload read (%s)", async (kind) => {
  vi.useFakeTimers();
  const client = new QueryClient();
  client.mount();
  const read = vi.fn<() => Promise<{ id: string }[]>>().mockRejectedValueOnce(new Error("first")).mockResolvedValue([{ id: "one" }]);
  const input = { queryClient: client, queryKey: ["recovers", kind], getKey: (row: { id: string }) => row.id };
  const collection = kind === "api"
    ? createApiCollection({ ...input, queryFn: read, refetchInterval: 15_000 })
    : createChangeCollection({ ...input, read: async () => ({ full: true as const, rows: await read(), cursor: "1" }) });
  const active = collection.subscribeChanges(() => {});
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.status).toBe("error");
  if (kind === "api") await vi.advanceTimersByTimeAsync(15_000);
  else await client.refetchQueries({ queryKey: input.queryKey });
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.status).toBe("ready");
  expect(collection.get("one")).toMatchObject({ id: "one" });
  // reload() starts a fresh Query entry, so recovery reads once more.
  expect(read).toHaveBeenCalledTimes(3);
  if (kind === "api") {
    await vi.advanceTimersByTimeAsync(15_000);
    expect(read).toHaveBeenCalledTimes(4);
  }
  active.unsubscribe();
  await collection.cleanup();
  client.unmount();
  client.clear();
});

it.each(["api", "change"] as const)("recovers through another caller's refetch without cancelling it (%s)", async (kind) => {
  const client = new QueryClient();
  const read = vi.fn<() => Promise<{ id: string }[]>>().mockRejectedValueOnce(new Error("first")).mockResolvedValue([{ id: "one" }]);
  const input = { queryClient: client, queryKey: ["caller", kind], getKey: (row: { id: string }) => row.id };
  const collection = kind === "api"
    ? createApiCollection({ ...input, queryFn: read })
    : createChangeCollection({ ...input, read: async () => ({ full: true as const, rows: await read(), cursor: "1" }) });
  const active = collection.subscribeChanges(() => {});
  await vi.waitFor(() => expect(collection.status).toBe("error"));
  const refetch = collection.utils.refetch();
  const preload = preloadCollection(collection);
  await expect(refetch).resolves.toBeDefined();
  await expect(preload).resolves.toBeUndefined();
  await vi.waitFor(() => expect(collection.status).toBe("ready"));
  expect(collection.get("one")).toMatchObject({ id: "one" });
  active.unsubscribe();
  await getDbClient(client).cleanup();
  client.clear();
});

function countCacheListeners(client: QueryClient) {
  const cache = client.getQueryCache();
  const subscribe = cache.subscribe.bind(cache);
  const live = { count: 0 };
  vi.spyOn(cache, "subscribe").mockImplementation((listener) => {
    live.count += 1;
    const unsubscribe = subscribe(listener);
    return () => { live.count -= 1; unsubscribe(); };
  });
  return live;
}

it("watches the Query cache only while a first load has failed, and again after a restart", async () => {
  const client = new QueryClient();
  const listeners = countCacheListeners(client);
  const read = vi.fn<() => Promise<{ id: string }[]>>().mockResolvedValue([{ id: "one" }]);
  const collection = createApiCollection({ queryClient: client, queryKey: ["watch"], queryFn: read, getKey: (row: { id: string }) => row.id });
  const failThenRecover = async () => {
    read.mockRejectedValueOnce(new Error("first"));
    await expect(preloadCollection(collection)).rejects.toThrow("first");
    expect(listeners.count).toBe(healthy + 1);
    const active = collection.subscribeChanges(() => {});
    await client.refetchQueries({ queryKey: ["watch"] });
    await vi.waitFor(() => expect(collection.status).toBe("ready"));
    expect(listeners.count).toBe(healthy);
    active.unsubscribe();
  };
  await preloadCollection(collection);
  const healthy = listeners.count;
  await collection.cleanup();
  await failThenRecover();
  await collection.cleanup();
  await failThenRecover();
  await collection.cleanup();
  read.mockRejectedValueOnce(new Error("again"));
  await expect(preloadCollection(collection)).rejects.toThrow("again");
  await getDbClient(client).cleanup();
  client.clear();
  expect(listeners.count).toBe(0);
  vi.restoreAllMocks();
});

it("releases preload-only and reconciliation observers while keeping mounted consumers active", async () => {
  vi.useFakeTimers();
  focusManager.setFocused(true);
  const client = new QueryClient();
  client.mount();
  const read = vi.fn(async () => [{ id: "loaded" }]);
  const collection = createApiCollection({ queryClient: client, queryKey: ["preload-only"], queryFn: read, refetchInterval: 15_000,
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
  expect(collection.health.hasErrors).toBe(true);
  expect(collection.subscriberCount).toBe(0);
  await collection.cleanup();
  client.clear();
});

it("merges incremental reads, drops deleted ids, and advances the cursor when nothing changed", async () => {
  const client = new QueryClient();
  type Row = { id: string; name: string };
  const read = vi.fn<(context: { since: string | undefined }) => Promise<CollectionRead<Row>>>()
    .mockResolvedValueOnce({ full: true, rows: [{ id: "a", name: "a" }, { id: "b", name: "b" }], cursor: "10" })
    .mockResolvedValueOnce({ full: false, rows: [{ id: "a", name: "renamed" }, { id: "c", name: "c" }], deleted: ["b"], cursor: "11" })
    .mockResolvedValueOnce({ full: false, rows: [], deleted: [], cursor: "12" })
    .mockResolvedValueOnce({ full: false, rows: [], deleted: [], cursor: "13" })
    .mockResolvedValueOnce({ full: true, rows: [{ id: "z", name: "z" }], cursor: "20" });
  const collection = createChangeCollection({ queryClient: client, queryKey: ["changes"], read, getKey: (row: Row) => row.id });
  const names = () => [...collection.values()].map((row) => row.name).sort();
  const active = collection.subscribeChanges(() => {});
  await preloadCollection(collection);
  expect(names()).toEqual(["a", "b"]);
  await collection.utils.refetch();
  expect(names()).toEqual(["c", "renamed"]);
  await collection.utils.refetch();
  expect(names()).toEqual(["c", "renamed"]);
  // An own write keeps the cursor; the next read starts after the quiet one.
  await collection.writeCommitted({ id: "d", name: "d" });
  await collection.utils.refetch();
  expect(names()).toEqual(["c", "d", "renamed"]);
  await collection.utils.refetch();
  expect(read.mock.calls.map(([context]) => context.since)).toEqual([undefined, "10", "11", "12", "13"]);
  expect(names()).toEqual(["z"]);
  active.unsubscribe();
  await collection.cleanup();
  client.clear();
});
