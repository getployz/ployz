// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { LIVE_LOG_LIMIT } from "./container-log.collection";
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
  stream.view();
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

it("closes the connection once no viewer is on screen, keeps the lines until DB garbage collection, and reopens on view", async () => {
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
  expect(sources).toHaveLength(0);
  const leave = stream.view();
  try {
    expect(sources).toHaveLength(1);
    stream.collection.insert({ kind: "line", level: "info", id: "1", timestamp: "1", machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", message: "hello" });
    expect(getContainerLogStream(selection, { ...scope, sessionId: "other" })).not.toBe(stream);
    expect(getContainerLogStream(selection, { ...scope, userId: "other" })).not.toBe(stream);
    expect(getContainerLogStream({ ...selection, organizationSlug: "other" }, scope)).not.toBe(stream);
    expect(getContainerLogStream({ ...selection, serviceId: "other" }, scope)).not.toBe(stream);
    expect(getContainerLogStream({ ...selection, environmentSlug: "production" }, scope)).not.toBe(stream);
    // A viewer that remounts at once keeps the connection.
    leave();
    const again = stream.view();
    await vi.advanceTimersByTimeAsync(0);
    expect(sources).toHaveLength(1);
    expect(sources[0]?.closed).toBe(false);
    again();
    subscription.unsubscribe();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(sources[0]?.closed).toBe(true);
    expect(stream.signal.aborted).toBe(true);
    expect(stream.collection.size).toBe(1);
    const returned = stream.view();
    expect(sources).toHaveLength(2);
    expect(stream.signal.aborted).toBe(false);
    returned();
    await vi.advanceTimersByTimeAsync(301_000);
    expect(stream.collection.size).toBe(0);
    expect(sources.every(source => source.closed)).toBe(true);
    const reopened = stream.collection.subscribeChanges(() => {});
    const viewing = stream.view();
    expect(sources).toHaveLength(3);
    viewing();
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
  stream.view();
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
  stream.view();
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

it("reads the newest page first for the lines the live tail lacks, then follows its cursor past the tail", async () => {
  vi.useFakeTimers();
  const sources: EventTarget[] = [];
  class FakeEventSource extends EventTarget {
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const line = (at: number, channel: "stdout" | "stderr" = "stdout") => ({
    kind: "line", id: `${channel}/${at}`, timestamp: String(at), machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel, level: "info", message: `line ${at}`,
  });
  const reads: string[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: string, init: RequestInit) => {
    const { cursor } = JSON.parse(String(init.body)) as { cursor?: string };
    reads.push(cursor ? `cursor=${cursor}` : "newest");
    const page = cursor ? { rows: [line(5)], cursor: null } : { rows: [line(20), line(30, "stderr")], cursor: "inside-tail" };
    return new Response(JSON.stringify({ ...page, failures: [] }));
  }));
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "exits" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  stream.view();
  try {
    for (const at of [10, 20]) sources.at(-1)?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: line(at) }) }));
    await vi.advanceTimersByTimeAsync(260);
    sources.at(-1)?.dispatchEvent(new MessageEvent("live"));
    await vi.waitFor(() => expect(stream.hasOlder).toBe(false));
    expect(reads).toEqual(["newest", "cursor=inside-tail"]);
    expect(stream.collection.has("stderr/30")).toBe(true);
    expect(stream.collection.has("stdout/5")).toBe(true);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

async function withFocusedStream(run: (ctx: { stream: ReturnType<typeof getContainerLogStream>; bodies: Array<{ service?: string; contains?: string; cursor?: string }>; sources: EventTarget[]; line: (service: string, at: number) => object }) => Promise<void>) {
  vi.useFakeTimers();
  const sources: EventTarget[] = [];
  class FakeEventSource extends EventTarget {
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const line = (service: string, at: number) => ({
    kind: "line", id: `${service}/${at}`, timestamp: String(at), machineId: "m", machineName: "Server", containerId: service, serviceName: service, channel: "stdout", level: "info", message: `${service} ${at}`,
  });
  const bodies: Array<{ service?: string; contains?: string; cursor?: string }> = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: string, init: RequestInit) => {
    const body = JSON.parse(String(init.body)) as { service?: string; contains?: string; cursor?: string };
    bodies.push(body);
    return new Response(JSON.stringify({ rows: [line(body.service ?? "any", body.contains ? 7 : 5)], cursor: null, failures: [] }));
  }));
  const queryClient = new QueryClient();
  const stream = getContainerLogStream({ organizationSlug: "focus", projectSlug: "shop", environmentSlug: "production" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  stream.view();
  try {
    sources.at(-1)?.dispatchEvent(new MessageEvent("live"));
    await vi.advanceTimersByTimeAsync(0);
    bodies.length = 0;
    await run({ stream, bodies, sources, line });
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
}

it("reads a focused Service's stored lines, keeps the lines already held, and stops reading for all Services", async () => {
  await withFocusedStream(async ({ stream, bodies, sources, line }) => {
    for (const at of [10, 20, 30]) sources.at(-1)?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: line("api", at) }) }));
    await vi.advanceTimersByTimeAsync(260);
    stream.focus("removed");
    await vi.waitFor(() => expect(stream.collection.has("removed/5")).toBe(true));
    expect(bodies.map(body => body.service)).toEqual(["removed"]);
    expect(stream.collection.has("api/30")).toBe(true);
    stream.focus("");
    await vi.advanceTimersByTimeAsync(0);
    expect(bodies).toHaveLength(1);
    expect(stream.collection.has("removed/5")).toBe(true);
  });
});

it("reads every Service's history for a search while no Service is focused, and the focused Service's again once the search clears", async () => {
  await withFocusedStream(async ({ stream, bodies }) => {
    stream.focus("removed");
    await vi.waitFor(() => expect(bodies).toHaveLength(1));
    stream.search("boom");
    await vi.waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toMatchObject({ contains: "boom", service: "removed" });
    stream.focus("");
    await vi.waitFor(() => expect(bodies).toHaveLength(3));
    expect(bodies[2]).toMatchObject({ contains: "boom" });
    expect(bodies[2]?.service).toBeUndefined();
    stream.focus("removed");
    await vi.waitFor(() => expect(bodies).toHaveLength(4));
    stream.search("");
    await vi.waitFor(() => expect(bodies).toHaveLength(5));
    expect(bodies[4]).toMatchObject({ service: "removed" });
    expect(bodies[4]?.contains).toBeUndefined();
  });
});

it("reads a Service's history though other Services' loaded lines fill the scrollback", async () => {
  await withFocusedStream(async ({ stream, bodies, line }) => {
    for (let at = 1; at <= 30_000; at++) stream.collection.insert(line("api", at) as never);
    stream.focus("removed");
    await vi.waitFor(() => expect(stream.collection.has("removed/5")).toBe(true));
    expect(bodies.map(body => body.service)).toEqual(["removed"]);
  });
});

type Stored = { container: string; at: number };
const PAGE = 500;

/**
 * Cloud's history holding `stored`, answered as the route does: newest first, a page at a time, and a cursor of the
 * oldest line's timestamp and hash. A hash tells identical lines apart.
 */
function storeFetch(stored: readonly Stored[], reads: string[]) {
  return async (_url: string, init: RequestInit) => {
    const rows = stored.map(({ container, at }, hash) => ({ container, at, hash }))
      .sort((a, b) => b.at - a.at || b.hash - a.hash);
    const { cursor } = JSON.parse(String(init.body)) as { cursor?: string };
    reads.push(cursor ? "cursor" : "newest");
    const [at, hash] = cursor?.split(".").map(Number) ?? [];
    const older = at === undefined || hash === undefined ? rows : rows.filter(row => row.at < at || (row.at === at && row.hash < hash));
    const page = older.slice(0, PAGE);
    const oldest = page.at(-1);
    return Response.json({
      rows: page.map(row => ({
        kind: "line", id: `m/${row.container}/${row.at}/h${row.hash}`, timestamp: String(row.at), machineId: "m", machineName: "Server",
        containerId: row.container, serviceName: row.container, channel: "stdout", level: "info", message: `${row.container} ${row.at}`,
      })),
      failures: [],
      cursor: oldest === undefined || page.length < PAGE ? null : `${oldest.at}.${oldest.hash}`,
    });
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
])("pages back through Cloud's history holding each line once: $name", async ({ stored, live }) => {
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
  stream.view();
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

it("holds each line once when lines waiting past the limit land after a read of Cloud's history", async () => {
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
  stream.view();
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

it("keeps every waiting line of a timestamp group Cloud's history had only part of when they overflow", async () => {
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
  const stream = getContainerLogStream({ organizationSlug: "partial-group", environmentSlug: "env", projectSlug: "p" }, { queryClient, sessionId: "session", userId: "user" });
  const subscription = stream.collection.subscribeChanges(() => {});
  stream.view();
  const seen = new Map<number, number>();
  const send = async (rows: readonly Stored[]) => {
    for (const { container, at } of rows) {
      const ordinal = seen.get(at) ?? 0;
      seen.set(at, ordinal + 1);
      source?.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
        kind: "line", id: `live/${container}/${at}/${ordinal}`, timestamp: String(at), machineId: "m", machineName: "Server", containerId: container, serviceName: container, channel: "stdout", level: "info", message: `${container} ${at}`,
      } }) }));
    }
    await vi.advanceTimersByTimeAsync(260);
  };
  try {
    await send(span("busy", 1, 300));
    stream.follow(false);
    const line = { container: "busy", at: 10_400 };
    // The Store is a moment behind: its newest page holds one of the three lines written in the same nanosecond.
    stored.push(...span("busy", 301, 10_399), line);
    await send([...span("busy", 301, 10_399), line, line, line]);
    await stream.loadOlder();
    stored.push(line, line, ...span("busy", 10_401, 10_500));
    await send(span("busy", 10_401, 10_500));
    stream.follow(true);
    expect([...stream.collection.values()].filter(row => row.timestamp === "10400")).toHaveLength(3);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});

it("drops a read of Cloud's history still in flight when the viewer returns to the newest lines past the limit", async () => {
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
  stream.view();
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

it("keeps each failed container's reason on a Server's missing note until every one sends again or leaves", async () => {
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
  stream.view();
  const send = (data: string) => source?.dispatchEvent(new MessageEvent("log", { data }));
  const record = (containerId: string) => JSON.stringify({ type: "record", record: {
    kind: "line", id: `${containerId}/1`, timestamp: "1", machineId: "m", machineName: "Server", containerId, serviceName: "api", channel: "stdout", level: "info", message: "back",
  } });
  try {
    send(JSON.stringify({ type: "source_error", machineId: "m", containerId: "c", message: "c is unavailable" }));
    send(JSON.stringify({ type: "source_error", machineId: "m", containerId: "d", message: "d is unavailable" }));
    send(record("other"));
    expect(stream.getSnapshot().missing.live["m"]?.failed).toEqual({ c: { message: "c is unavailable" }, d: { message: "d is unavailable" } });
    send(record("d"));
    expect(stream.getSnapshot().missing.live["m"]?.failed).toEqual({ c: { message: "c is unavailable" } });
    send(JSON.stringify({ type: "source_gone", machineId: "m", containerId: "c" }));
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
  stream.view();
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
  stream.view();
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
  stream.view();
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
