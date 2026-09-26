// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { getContainerLogStream } from "./container-log.stream";

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
    stream.collection.insert({ id: "1", timestamp: "1", machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", message: "hello" });
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
    const reopened = stream.collection.subscribeChanges(() => {});
    expect(sources).toHaveLength(2);
    expect(stream.signal.aborted).toBe(false);
    reopened.unsubscribe();
  } finally {
    subscription.unsubscribe();
    await stream.collection.cleanup();
    queryClient.clear(); vi.useRealTimers(); vi.unstubAllGlobals();
  }
});
