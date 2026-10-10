// @vitest-environment jsdom
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import type { LogHistoryOptions, LogHistoryPage } from "@ployz/sdk";
import { LIVE_LOG_LIMIT, projectContainerLog } from "./container-log.collection";
import { getContainerLogStream } from "./container-log.stream";

it("delivers a log burst together without losing or duplicating replayed records", async () => {
  vi.useFakeTimers();
  let source: EventTarget | undefined;
  class FakeEventSource extends EventTarget {
    constructor() { super(); source = this; }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "burst" }, { queryClient, sessionId: "session", userId: "user" });
  const changes = vi.fn();
  const subscription = stream.collection.subscribeChanges(changes);
  try {
    const baseline = changes.mock.calls.length;
    for (let i = 0; i < 3_001; i++) {
      source?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
        kind: "line", id: String(i % 3_000), timestamp: String(i % 3_000), machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", level: "info", message: `line ${i % 3_000}`,
      } }) }));
    }
    expect(stream.collection.size).toBe(0);
    await vi.advanceTimersByTimeAsync(260);
    expect(stream.collection.size).toBe(3_000);
    expect(changes.mock.calls.length - baseline).toBeLessThanOrEqual(2);
    expect(stream.collection.get("2999")).toMatchObject({ message: "line 2999" });
    source?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
      kind: "line", id: "pending", timestamp: "3000", machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", level: "info", message: "pending",
    } }) }));
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    await vi.advanceTimersByTimeAsync(260);
    expect(stream.collection.has("pending")).toBe(false);
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("retains an inactive stream until DB garbage collection, then reopens it on demand", async () => {
  vi.useFakeTimers();
  const sources: FakeEventSource[] = [];
  class FakeEventSource extends EventTarget {
    closed = false;
    constructor() { super(); sources.push(this); }
    close() { this.closed = true; }
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const selection = { organizationSlug: "acme", serviceId: "api" };
  const stream = getContainerLogStream(selection, scope);
  expect(sources).toHaveLength(0);
  const subscription = stream.collection.subscribeChanges(() => {});
  try {
    expect(sources).toHaveLength(1);
    stream.collection.insert({ kind: "line", level: "info", id: "1", timestamp: "1", machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", message: "hello" });
    expect(getContainerLogStream(selection, { ...scope, sessionId: "other" })).not.toBe(stream);
    expect(getContainerLogStream(selection, { ...scope, userId: "other" })).not.toBe(stream);
    expect(getContainerLogStream({ ...selection, organizationSlug: "other" }, scope)).not.toBe(stream);
    expect(getContainerLogStream({ ...selection, serviceId: "other" }, scope)).not.toBe(stream);
    expect(getContainerLogStream({ ...selection, environmentSlug: "production" }, scope)).not.toBe(stream);
    subscription.unsubscribe();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(stream.collection.size).toBe(1);
    expect(sources[0]?.closed).toBe(false);
    await vi.advanceTimersByTimeAsync(301_000);
    expect(stream.collection.size).toBe(0);
    expect(sources[0]?.closed).toBe(true);
    expect(stream.signal.aborted).toBe(true);
    // A silent stream reconnects each minute on its own; reopening adds exactly one more.
    const opened = sources.length;
    const reopened = stream.collection.subscribeChanges(() => {});
    expect(sources).toHaveLength(opened + 1);
    expect(stream.signal.aborted).toBe(false);
    reopened.unsubscribe();
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("holds live lines while the viewer reads older ones, and past the limit starts over from the newest", async () => {
  vi.useFakeTimers();
  const sources: EventTarget[] = [];
  class FakeEventSource extends EventTarget {
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "held" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  const send = (at: number) => sources.at(-1)?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
    kind: "line", id: String(at), timestamp: String(at), machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", level: "info", message: `line ${at}`,
  } }) }));
  try {
    send(1);
    await vi.advanceTimersByTimeAsync(260);
    stream.follow(false);
    send(2);
    await vi.advanceTimersByTimeAsync(260);
    expect([...stream.collection.keys()]).toEqual(["1"]);
    stream.follow(true);
    expect([...stream.collection.keys()].sort()).toEqual(["1", "2"]);
    stream.follow(false);
    for (let at = 3; at <= LIVE_LOG_LIMIT + 3; at++) send(at);
    await vi.advanceTimersByTimeAsync(260);
    expect(stream.collection.size).toBe(2);
    stream.follow(true);
    expect(stream.collection.size).toBe(LIVE_LOG_LIMIT);
    expect(stream.collection.has("2")).toBe(false);
    expect(stream.collection.has("3")).toBe(false);
    expect(stream.collection.has(String(LIVE_LOG_LIMIT + 3))).toBe(true);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("keeps the newest waiting lines by time when a slower Server's older lines arrive last", async () => {
  vi.useFakeTimers();
  const sources: EventTarget[] = [];
  class FakeEventSource extends EventTarget {
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "skewed" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  const send = (id: string, at: number, machineId: string) => sources.at(-1)?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
    kind: "line", id, timestamp: String(at), machineId, machineName: machineId, containerId: machineId, serviceName: "api", channel: "stdout", level: "info", message: id,
  } }) }));
  try {
    stream.follow(false);
    send("fast", LIVE_LOG_LIMIT * 2, "fast");
    for (let at = 1; at <= LIVE_LOG_LIMIT; at++) send(`slow-${at}`, at, "slow");
    await vi.advanceTimersByTimeAsync(260);
    stream.follow(true);
    expect(stream.collection.size).toBe(LIVE_LOG_LIMIT);
    expect(stream.collection.has("fast")).toBe(true);
    expect(stream.collection.has("slow-1")).toBe(false);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("reads the newest page first for the exits the live tail lacks, then follows its cursor past the tail", async () => {
  vi.useFakeTimers();
  const sources: EventTarget[] = [];
  class FakeEventSource extends EventTarget {
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const line = (at: number, channel: "stdout" | "lifecycle" = "stdout") => ({
    kind: "line", id: `${channel}/${at}`, timestamp: String(at), machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel, level: "info", message: `line ${at}`,
  });
  const reads: string[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: string, init: RequestInit) => {
    const { cursor } = JSON.parse(String(init.body)) as { cursor?: string };
    reads.push(cursor ? `cursor=${cursor}` : "newest");
    const page = cursor ? { rows: [line(5)], cursor: null } : { rows: [line(20), line(30, "lifecycle")], cursor: "inside-tail" };
    return new Response(JSON.stringify({ ...page, failures: [] }));
  }));
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "exits" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  try {
    for (const at of [10, 20]) sources.at(-1)?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: line(at) }) }));
    await vi.advanceTimersByTimeAsync(260);
    sources.at(-1)?.dispatchEvent(new MessageEvent("live"));
    await vi.waitFor(() => expect(stream.hasOlder).toBe(false));
    expect(reads).toEqual(["newest", "cursor=inside-tail"]);
    expect(stream.collection.has("lifecycle/30")).toBe(true);
    expect(stream.collection.has("stdout/5")).toBe(true);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

type Stored = { container: string; at: number };
type HistoryRead = { cursor: string | null; until_nanos: string | null; limit: number };
type StoreTransport = { watch(): AsyncGenerator<object>; history(input: HistoryRead): Promise<{ next(): Promise<object | null>; cancel(): void }> };
const sdkHistory = createRequire(import.meta.url)(join(dirname(createRequire(import.meta.url).resolve("@ployz/sdk")), "runtime-logs.js")).history as (transport: StoreTransport, options: LogHistoryOptions) => Promise<LogHistoryPage>;

/**
 * One Server's Log Store holding `stored`, read through the SDK's history and the route's projection. `stored` may
 * grow between reads; a cursor counts the rows older than its page's end, so newer lines don't shift it.
 */
function storeFetch(stored: readonly Stored[], reads: string[]) {
  const transport: StoreTransport = {
    async *watch() { yield { machines: [{ machine: { id: "m", name: "Server" } }], containers: [] }; },
    async history(input) {
      const rows = stored.map(({ container, at }) => ({ row: "line", container_id: container, timestamp_nanos: String(at), stream: "stdout", level: "info", message: `${container} ${at}` }))
        .sort((a, b) => Number(b.timestamp_nanos) - Number(a.timestamp_nanos));
      const containers = [...new Set(stored.map(row => row.container))];
      const from = input.cursor === null ? 0 : rows.length - Number(input.cursor);
      const selected = rows.slice(from).filter(row => input.until_nanos === null || BigInt(row.timestamp_nanos) < BigInt(input.until_nanos));
      const page = selected.slice(0, input.limit);
      const output: object[] = [
        ...containers.map(container_id => ({ row: "container", container_id, namespace: "env", service: container_id, replica: container_id, kind: "service" })),
        ...page, { row: "end", next: page.length < selected.length ? String(selected.length - page.length) : null },
      ];
      return { async next() { return output.shift() ?? null; }, cancel() {} };
    },
  };
  return async (_url: string, init: RequestInit) => {
    const { cursor, before } = JSON.parse(String(init.body)) as { cursor?: string; before?: string };
    reads.push(cursor ? "cursor" : before === undefined ? "newest" : `before=${before}`);
    const page = await sdkHistory(transport, { filter: { namespace: "env" }, limit: 500, cursor, before });
    return Response.json({ rows: page.records.map(projectContainerLog), failures: page.failures, cursor: page.cursor });
  };
}

const span = (container: string, from: number, to: number, step = 1) =>
  Array.from({ length: Math.floor((to - from) / step) + 1 }, (_, i) => ({ container, at: from + i * step }));

it.each([
  { name: "identical lines split across store pages", stored: Array.from({ length: 501 }, () => ({ container: "same", at: 100 })), live: Array.from({ length: 20 }, () => ({ container: "same", at: 100 })) },
  // A new container's only line sits inside the busy one's newest page.
  { name: "a new container's tail starts late", stored: [...span("busy", 1, 600), { container: "fresh", at: 599 }, { container: "quiet", at: 1 }], live: [...span("busy", 401, 600), { container: "fresh", at: 599 }, { container: "quiet", at: 1 }] },
  // After a deploy: the old container is only in the Store, the new one's 50 lines are live, beside a quiet Service.
  { name: "a deploy replaced the container", stored: [...span("old", 10_001, 10_450), ...span("new", 10_451, 10_500), { container: "quiet", at: 1 }], live: [...span("new", 10_451, 10_500), { container: "quiet", at: 1 }] },
  // Steady: a container writing every tenth, a busy one, a quiet one. The newest page ends on a sparse line the busy
  // container shares a timestamp with.
  { name: "busy, sparse and quiet containers", stored: [...span("sparse", 10, 2_000, 10), ...span("busy", 1, 2_004), { container: "quiet", at: 1 }], live: [...span("sparse", 10, 2_000, 10), ...span("busy", 1_805, 2_004), { container: "quiet", at: 1 }] },
  // A rolling deploy mid-flight: the stopped replica's last lines overlap its replacement's first, inside the live tail.
  {
    name: "a rolling deploy's stopped replica",
    stored: [...["a1", "a2", "a3", "a4"].flatMap(name => span(name, 1, 2_000)), ...span("bold", 1, 1_897), ...span("bnew", 1_890, 2_000)],
    live: [...["a1", "a2", "a3", "a4"].flatMap(name => span(name, 1_801, 2_000)), ...span("bnew", 1_890, 2_000)],
  },
  // Three busy replicas and a container that stopped after writing a few lines inside their live tail.
  {
    name: "a stopped container inside the busy replicas' tail",
    stored: [...["r1", "r2", "r3"].flatMap(name => span(name, 1, 1_500)), ...span("x", 1_010, 1_020)],
    live: [...["r1", "r2", "r3"].flatMap(name => span(name, 1_000, 1_500))],
  },
])("pages back through the Log Store holding each line once: $name", async ({ stored, live }) => {
  vi.useFakeTimers();
  let source: EventTarget | undefined;
  class FakeEventSource extends EventTarget {
    constructor() { super(); source = this; }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const reads: string[] = [];
  vi.stubGlobal("fetch", storeFetch(stored, reads));
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "paging", environmentSlug: "env", projectSlug: "p" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  try {
    for (const { container, at } of live) {
      source?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
        kind: "line", id: `live/${container}/${at}`, timestamp: String(at), machineId: "m", machineName: "Server", containerId: container, serviceName: container, channel: "stdout", level: "info", message: `${container} ${at}`,
      } }) }));
    }
    await vi.advanceTimersByTimeAsync(260);
    for (let scroll = 0; scroll < 50 && stream.hasOlder; scroll++) await stream.loadOlder();
    expect(stream.hasOlder).toBe(false);
    const lines = [...stream.collection.values()].map(row => `${row.containerId} ${row.timestamp}`).sort();
    expect(lines).toEqual(stored.map(({ container, at }) => `${container} ${at}`).sort());
    expect(reads.filter(read => read !== "cursor")).toEqual(["newest"]);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("holds each line once when lines waiting past the limit land after a read of the Log Store", async () => {
  vi.useFakeTimers();
  let source: EventTarget | undefined;
  class FakeEventSource extends EventTarget {
    constructor() { super(); source = this; }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const stored = span("busy", 1, 300);
  vi.stubGlobal("fetch", storeFetch(stored, []));
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "overflow", environmentSlug: "env", projectSlug: "p" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  const send = async (rows: readonly Stored[]) => {
    for (const { container, at } of rows) {
      source?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
        kind: "line", id: `live/${container}/${at}`, timestamp: String(at), machineId: "m", machineName: "Server", containerId: container, serviceName: container, channel: "stdout", level: "info", message: `${container} ${at}`,
      } }) }));
    }
    await vi.advanceTimersByTimeAsync(260);
  };
  try {
    // A tail this long reads no history on its own; the viewer scrolls up, reads the Store, and the waiting lines overflow.
    await send(span("busy", 1, 300));
    stream.follow(false);
    const waiting = span("busy", 301, 5_300);
    stored.push(...waiting);
    await send(waiting);
    await stream.loadOlder();
    const flood = span("busy", 5_301, 11_400);
    stored.push(...flood);
    await send(flood);
    stream.follow(true);
    const lines = [...stream.collection.values()].map(row => `${row.containerId} ${row.timestamp}`).sort();
    expect(lines).toEqual(span("busy", 1_401, 11_400).map(({ container, at }) => `${container} ${at}`).sort());
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("drops a read of the Log Store still in flight when the viewer returns to the newest lines past the limit", async () => {
  vi.useFakeTimers();
  let source: EventTarget | undefined;
  class FakeEventSource extends EventTarget {
    constructor() { super(); source = this; }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const stored = span("busy", 1, 1_300);
  const store = storeFetch(stored, []);
  let held: Promise<void> | undefined;
  vi.stubGlobal("fetch", async (url: string, init: RequestInit) => { await held; return store(url, init); });
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "stale-read", environmentSlug: "env", projectSlug: "p" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  const send = async (rows: readonly Stored[]) => {
    for (const { container, at } of rows) {
      source?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
        kind: "line", id: `live/${container}/${at}`, timestamp: String(at), machineId: "m", machineName: "Server", containerId: container, serviceName: container, channel: "stdout", level: "info", message: `${container} ${at}`,
      } }) }));
    }
    await vi.advanceTimersByTimeAsync(260);
  };
  try {
    await send(span("busy", 801, 1_300));
    await stream.loadOlder();
    stream.follow(false);
    const flood = span("busy", 1_301, 11_400);
    stored.push(...flood);
    await send(flood);
    let release = () => {};
    held = new Promise(resolve => { release = resolve; });
    const stale = stream.loadOlder();
    stream.follow(true);
    release();
    await stale;
    held = undefined;
    for (let scroll = 0; scroll < 50 && stream.hasOlder; scroll++) await stream.loadOlder();
    const lines = [...stream.collection.values()].map(row => `${row.containerId} ${row.timestamp}`).sort();
    expect(lines).toEqual(stored.map(({ container, at }) => `${container} ${at}`).sort());
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("clears a Server's missing note once every container that failed sends again", async () => {
  let source: EventTarget | undefined;
  class FakeEventSource extends EventTarget {
    constructor() { super(); source = this; }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  vi.stubGlobal("fetch", async () => Response.json({ rows: [], failures: [], cursor: null }));
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "recovers" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  const send = (data: string) => source?.dispatchEvent(new MessageEvent("log", { data }));
  const record = (containerId: string) => JSON.stringify({ type: "record", record: {
    kind: "line", id: `${containerId}/1`, timestamp: "1", machineId: "m", machineName: "Server", containerId, serviceName: "api", channel: "stdout", level: "info", message: "back",
  } });
  try {
    send(JSON.stringify({ type: "source_error", machineId: "m", containerId: "c", message: "unavailable" }));
    send(JSON.stringify({ type: "source_error", machineId: "m", containerId: "d", message: "unavailable" }));
    send(record("other"));
    expect(stream.getSnapshot().missing.live["m"]?.containerIds).toEqual(["c", "d"]);
    send(record("c"));
    expect(stream.getSnapshot().missing.live["m"]?.containerIds).toEqual(["d"]);
    send(record("d"));
    expect(stream.getSnapshot().missing.live).toEqual({});
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.unstubAllGlobals();
  }
});


it("retries a failed cursor page after its Server recovers", async () => {
  class FakeEventSource extends EventTarget { close() {} }
  vi.stubGlobal("EventSource", FakeEventSource);
  let recovered = false;
  const reads: string[] = [];
  const line = (at: number) => ({ kind: "line", id: String(at), timestamp: String(at), machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", level: "info", message: String(at) });
  vi.stubGlobal("fetch", async (_url: string, init: RequestInit) => {
    const { cursor } = JSON.parse(String(init.body)) as { cursor?: string };
    reads.push(cursor ?? "newest");
    return Response.json(cursor === undefined ? { rows: [line(100)], cursor: "retry", failures: [] }
      : recovered ? { rows: [line(50)], cursor: null, failures: [] }
      : { rows: [], cursor: "retry", failures: [{ machineId: "m", machineName: "Server", message: "unavailable" }] });
  });
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "retry-history" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  try {
    await stream.loadOlder();
    await stream.loadOlder();
    expect(stream.getSnapshot().missing.history).toHaveLength(1);
    const attempted = reads.length;
    recovered = true;
    await stream.loadOlder();
    expect(reads.length).toBeGreaterThan(attempted);
    expect(stream.collection.has("50")).toBe(true);
    expect(stream.getSnapshot().missing.history).toEqual([]);
    expect(stream.hasOlder).toBe(false);
  } finally {
    subscription.unsubscribe(); await stream.collection.cleanup();
    queryClient.clear(); vi.unstubAllGlobals();
  }
});

it("clears an aborted history read so the reconnected stream can load again", async () => {
  vi.useFakeTimers();
  const sources: FakeEventSource[] = [];
  class FakeEventSource extends EventTarget {
    static CLOSED = 2;
    readyState = FakeEventSource.CLOSED;
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  let first = true;
  const fetcher = vi.fn(async (_url: string, { signal }: { signal: AbortSignal }) => {
    if (!first) return Response.json({ rows: [], failures: [], cursor: null });
    first = false;
    return new Promise<Response>((_resolve, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true }));
  });
  vi.stubGlobal("fetch", fetcher);
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "abort-history" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  try {
    const loading = stream.loadOlder();
    expect(stream.getSnapshot().historyPending).toBe(true);
    sources.at(-1)?.dispatchEvent(new Event("error"));
    await loading;
    expect(stream.getSnapshot().historyPending).toBe(false);
    expect(stream.getSnapshot().historyError).toBe(false);
    await vi.advanceTimersByTimeAsync(1_000);
    sources.at(-1)?.dispatchEvent(new Event("open"));
    await stream.loadOlder();
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(stream.hasOlder).toBe(false);
  } finally {
    subscription.unsubscribe(); await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});


it("a late aborted read cannot clear a new connection's pending history", async () => {
  vi.useFakeTimers();
  const sources: FakeEventSource[] = [];
  class FakeEventSource extends EventTarget {
    static CLOSED = 2;
    readyState = FakeEventSource.CLOSED;
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const reads: { resolve: (value: Response) => void; reject: (reason: Error) => void }[] = [];
  vi.stubGlobal("fetch", () => new Promise<Response>((resolve, reject) => reads.push({ resolve, reject })));
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "late-abort-history" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  try {
    const oldRead = stream.loadOlder();
    sources.at(-1)?.dispatchEvent(new Event("error"));
    expect(stream.getSnapshot().historyPending).toBe(false);
    await stream.loadOlder();
    expect(reads).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1_000);
    sources.at(-1)?.dispatchEvent(new Event("open"));
    const newRead = stream.loadOlder();
    await vi.waitFor(() => expect(reads).toHaveLength(2));
    reads[0]?.reject(new Error("aborted old connection"));
    await oldRead;
    expect(stream.getSnapshot().historyPending).toBe(true);
    expect(stream.getSnapshot().historyError).toBe(false);
    reads[1]?.resolve(Response.json({ rows: [], failures: [], cursor: null }));
    await newRead;
    expect(stream.getSnapshot().historyPending).toBe(false);
    expect(stream.hasOlder).toBe(false);
  } finally {
    subscription.unsubscribe(); await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});
