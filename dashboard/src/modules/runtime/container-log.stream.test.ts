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

it("reads the newest page first for the exits the live tail lacks, then jumps before a tail that page sat inside", async () => {
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
    const params = JSON.parse(String(init.body)) as { cursor?: string; before?: string };
    reads.push(params.before ? `before=${params.before}` : params.cursor ? `cursor=${params.cursor}` : "newest");
    const page = params.before ? { rows: [line(5)], cursor: null } : { rows: [line(20), line(30, "lifecycle")], cursor: "inside-tail" };
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
    expect(reads).toEqual(["newest", "before=10"]);
    expect(stream.collection.has("lifecycle/30")).toBe(true);
    expect(stream.collection.has("stdout/5")).toBe(true);
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});
