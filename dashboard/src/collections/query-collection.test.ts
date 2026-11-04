// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { focusManager, onlineManager, QueryClient } from "@tanstack/react-query";
import { createApiCollection, createChangeCollection, preloadCollection, preloadCollections, reconcileCollection } from "./query-collection";
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
  read.mockRejectedValue(new Error("unavailable"));
  // The interval read and its three retries, 1s, 2s and 4s apart.
  await vi.advanceTimersByTimeAsync(23_000);
  expect(collection.get("b")?.name).toBe("after");
  expect(collection.health.hasErrors).toBe(true);
  read.mockImplementation(async () => rows);
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
  vi.useFakeTimers();
  const refresh = expect(collection.utils.refetch({ throwOnError: true })).rejects.toBe(failure);
  await vi.advanceTimersByTimeAsync(10_000);
  await refresh;
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

function failingFirstLoad(kind: "api" | "change", name: string) {
  const client = new QueryClient();
  client.mount();
  const read = vi.fn<() => Promise<{ id: string }[]>>().mockRejectedValueOnce(new Error("first")).mockResolvedValue([{ id: "one" }]);
  const input = { queryClient: client, queryKey: [name, kind], getKey: (row: { id: string }) => row.id };
  const collection = kind === "api"
    ? createApiCollection({ ...input, queryFn: read, refetchInterval: 15_000 })
    : createChangeCollection({ ...input, read: async () => ({ full: true as const, rows: await read(), cursor: "1" }) });
  const end = async () => {
    await getDbClient(client).cleanup();
    client.unmount();
    client.clear();
  };
  return { client, collection, read, queryKey: input.queryKey, end };
}

it.each(["api", "change"] as const)("recovery waits for a read started right after the one it saw succeed (%s)", async (kind) => {
  vi.useFakeTimers();
  const { client, collection, read, end } = failingFirstLoad(kind, "later-read");
  const active = collection.subscribeChanges(() => {});
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.status).toBe("error");
  let resolve!: (rows: { id: string }[]) => void;
  read.mockResolvedValueOnce([{ id: "one" }]).mockImplementationOnce(() => new Promise((settle) => { resolve = settle; }));
  let slow: Promise<unknown> | undefined;
  const stop = client.getQueryCache().subscribe((event) => {
    if (event.type !== "updated" || event.action.type !== "success") return;
    stop();
    queueMicrotask(() => { slow = collection.utils.refetch(); });
  });
  await collection.utils.refetch();
  // Past recovery's first wait, so it sees the slow read still running.
  await vi.advanceTimersByTimeAsync(20);
  resolve([{ id: "one" }]);
  await expect(slow).resolves.toBeDefined();
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.status).toBe("ready");
  active.unsubscribe();
  await end();
});

it.each(["api", "change"] as const)("a cleanup while recovery waits ends it without a read (%s)", async (kind) => {
  vi.useFakeTimers();
  const { client, collection, read, queryKey, end } = failingFirstLoad(kind, "cleanup-recovery");
  await expect(preloadCollection(collection)).rejects.toThrow("first");
  await client.refetchQueries({ queryKey });
  await collection.cleanup();
  const reads = read.mock.calls.length;
  await vi.advanceTimersByTimeAsync(10);
  expect(collection.status).toBe("cleaned-up");
  expect(read).toHaveBeenCalledTimes(reads);
  await end();
});

it.each(["api", "change"] as const)("a retry holds its Collection while another read settles (%s)", async (kind) => {
  vi.useFakeTimers();
  const { collection, read, end } = failingFirstLoad(kind, "held-recovery");
  await expect(preloadCollection(collection)).rejects.toThrow("first");
  const statuses: string[] = [];
  collection.on("status:change", ({ status }) => { statuses.push(status); });
  let resolve!: (rows: { id: string }[]) => void;
  read.mockImplementationOnce(() => new Promise((settle) => { resolve = settle; }));
  const refetch = collection.utils.refetch();
  const retry = preloadCollection(collection);
  // Past the Collection's 5 minute GC time.
  await vi.advanceTimersByTimeAsync(300_100);
  resolve([{ id: "one" }]);
  await refetch;
  await vi.advanceTimersByTimeAsync(10);
  await expect(retry).resolves.toBeUndefined();
  expect(statuses).not.toContain("cleaned-up");
  expect(collection.status).toBe("ready");
  expect(read).toHaveBeenCalledTimes(3);
  await end();
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

it.each([false, true])("applies committed rows without an observer over older reads (preloaded: %s)", async (preloaded) => {
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
  await collection.utils.writeUpsert(committed);
  finishRead([stale]);
  await pendingRead;
  expect(collection.get("row")).toMatchObject(committed);
  active.unsubscribe();
  read.mockRejectedValue(new Error("GET unavailable"));
  vi.useFakeTimers();
  const reconciled = reconcileCollection(collection);
  await vi.advanceTimersByTimeAsync(10_000);
  await reconciled;
  expect(collection.get("row")).toMatchObject(committed);
  expect(collection.health.hasErrors).toBe(true);
  expect(collection.subscriberCount).toBe(0);
  await collection.cleanup();
  client.clear();
});

it("does not stall a first load when a committed write lands during it", async () => {
  const client = new QueryClient();
  const stale = { id: "row", revision: "old" };
  let finishRead = (_rows: typeof stale[]) => {};
  const read = vi.fn(() => new Promise<typeof stale[]>((resolve) => { finishRead = resolve; }));
  const collection = createApiCollection({ queryClient: client, queryKey: ["committed", "first-load"], queryFn: read,
    getKey: (row: typeof stale) => row.id });
  const loading = preloadCollection(collection);
  await vi.waitFor(() => expect(read).toHaveBeenCalled());
  await collection.utils.writeUpsert({ id: "row", revision: "committed" });
  finishRead([stale]);
  await loading;
  expect(collection.get("row")).toMatchObject({ revision: "committed" });
  await collection.cleanup();
  client.clear();
});

it("keeps a committed row over an incremental read that started before the write", async () => {
  const client = new QueryClient();
  type Row = { id: string; name: string };
  let finishRead = (_result: CollectionRead<Row>) => {};
  const read = vi.fn<(context: { since: string | undefined }) => Promise<CollectionRead<Row>>>()
    .mockResolvedValueOnce({ full: true, rows: [{ id: "a", name: "a" }], cursor: "10" })
    .mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; }))
    .mockResolvedValueOnce({ full: false, rows: [], deleted: [], cursor: "12" });
  const collection = createChangeCollection({ queryClient: client, queryKey: ["changes", "race"], read, getKey: (row: Row) => row.id });
  const names = () => [...collection.values()].map((row) => row.name).sort();
  const active = collection.subscribeChanges(() => {});
  await preloadCollection(collection);
  const pendingRead = collection.utils.refetch();
  await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(2));
  await collection.utils.writeUpsert({ id: "d", name: "d" });
  finishRead({ full: false, rows: [], deleted: [], cursor: "11" });
  await pendingRead;
  expect(names()).toEqual(["a", "d"]);
  await collection.utils.refetch();
  expect(read.mock.calls.map(([context]) => context.since)).toEqual([undefined, "10", "11"]);
  expect(names()).toEqual(["a", "d"]);
  active.unsubscribe();
  await collection.cleanup();
  client.clear();
});

it("shows a later reader the committed row when an unobserved refetch was in flight across the write", async () => {
  const client = new QueryClient();
  type Row = { id: string; name: string };
  let finishRead = (_result: CollectionRead<Row>) => {};
  const read = vi.fn<(context: { since: string | undefined }) => Promise<CollectionRead<Row>>>()
    .mockResolvedValueOnce({ full: true, rows: [{ id: "a", name: "old" }], cursor: "10" })
    .mockImplementationOnce(() => new Promise((resolve) => { finishRead = resolve; }))
    .mockResolvedValue({ full: false, rows: [], deleted: [], cursor: "12" });
  const collection = createChangeCollection({ queryClient: client, queryKey: ["changes", "unobserved"], read, getKey: (row: Row) => row.id });
  await preloadCollection(collection);
  const pendingRead = collection.utils.refetch();
  await vi.waitFor(() => expect(read).toHaveBeenCalledTimes(2));
  await collection.utils.writeUpsert({ id: "a", name: "committed" });
  finishRead({ full: false, rows: [], deleted: [], cursor: "11" });
  await pendingRead;
  const seen: string[] = [];
  const reader = collection.subscribeChanges((changes) => {
    for (const change of changes) seen.push(change.value.name);
  }, { includeInitialState: true });
  await preloadCollection(collection);
  await collection.utils.refetch();
  expect(seen).toEqual(["committed"]);
  expect(collection.get("a")?.name).toBe("committed");
  reader.unsubscribe();
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
  await collection.utils.writeUpsert({ id: "d", name: "d" });
  await collection.utils.refetch();
  expect(names()).toEqual(["c", "d", "renamed"]);
  await collection.utils.refetch();
  expect(read.mock.calls.map(([context]) => context.since)).toEqual([undefined, "10", "11", "12", "13"]);
  expect(names()).toEqual(["z"]);
  active.unsubscribe();
  await collection.cleanup();
  client.clear();
});

it.each(["api", "change"] as const)("retries a failed refresh with backoff, one read at a time, and stops after three tries (%s)", async (kind) => {
  vi.useFakeTimers();
  const client = new QueryClient();
  const read = vi.fn<() => Promise<{ id: string }[]>>().mockResolvedValue([{ id: "before" }]);
  const input = { queryClient: client, queryKey: ["refresh-retry", kind], getKey: (row: { id: string }) => row.id };
  const collection = kind === "api"
    ? createApiCollection({ ...input, queryFn: read })
    : createChangeCollection({ ...input, read: async () => ({ full: true as const, rows: await read(), cursor: "1" }) });
  const active = collection.subscribeChanges(() => {});
  await vi.advanceTimersByTimeAsync(10);
  read.mockRejectedValueOnce(new Error("one")).mockRejectedValueOnce(new Error("two")).mockResolvedValue([{ id: "after" }]);
  void collection.utils.refetch();
  await vi.advanceTimersByTimeAsync(10);
  expect(read).toHaveBeenCalledTimes(2);
  await vi.advanceTimersByTimeAsync(999);
  expect(read).toHaveBeenCalledTimes(3);
  await vi.advanceTimersByTimeAsync(2_000);
  expect(read).toHaveBeenCalledTimes(4);
  expect(collection.get("after")).toMatchObject({ id: "after" });
  expect(client.getQueryState(input.queryKey)?.status).toBe("success");
  read.mockRejectedValue(new Error("down"));
  void collection.utils.refetch();
  await vi.advanceTimersByTimeAsync(60_000);
  expect(read).toHaveBeenCalledTimes(8);
  expect(client.getQueryState(input.queryKey)?.status).toBe("error");
  expect(collection.get("after")).toMatchObject({ id: "after" });
  active.unsubscribe();
  await collection.cleanup();
  client.clear();
});

it("fails readiness only once every collection's read has settled, so SSR never streams a read it gave up on", async () => {
  vi.useFakeTimers();
  const client = new QueryClient();
  const getKey = (row: { id: string }) => row.id;
  const failing = createApiCollection({ queryClient: client, queryKey: ["ready", "failing"], getKey,
    queryFn: async () => { throw new Error("Read failed"); } });
  const slow = createApiCollection({ queryClient: client, queryKey: ["ready", "slow"], getKey,
    queryFn: () => new Promise<{ id: string }[]>((resolve) => setTimeout(() => resolve([{ id: "slow" }]), 1_000)) });
  let settled = false;
  const ready = preloadCollections([failing, slow]).finally(() => { settled = true; });
  const failure = expect(ready).rejects.toThrow("Read failed");
  await vi.advanceTimersByTimeAsync(500);
  expect(client.getQueryState(["ready", "failing"])?.status).toBe("error");
  expect(settled).toBe(false);
  await vi.advanceTimersByTimeAsync(500);
  await failure;
  expect(client.isFetching()).toBe(0);
  await failing.cleanup();
  await slow.cleanup();
  client.clear();
});
