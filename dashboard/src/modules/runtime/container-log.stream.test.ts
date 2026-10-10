// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
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
