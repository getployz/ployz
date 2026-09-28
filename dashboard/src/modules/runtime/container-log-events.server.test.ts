import { expect, it, vi } from "vitest";
import type { LogEvent } from "@ployz/sdk";
import { backfillThenFollow, containerLogResponse } from "./container-log-events.server";

it("answers live only after the whole tail, holding what the follow read meanwhile", async () => {
  const record = (message: string): LogEvent => ({ type: "source_error", machineId: "m", containerId: "c", message });
  let finishTail = () => {};
  const tailFinished = new Promise<void>(resolve => { finishTail = resolve; });
  async function* tail(): AsyncIterable<LogEvent> {
    yield record("tail");
    await tailFinished;
  }
  async function* follow(): AsyncIterable<LogEvent> {
    // Written while the tail was still draining: without the follow already running, it would fall between the reads.
    yield record("during tail");
    finishTail();
    yield record("after");
  }
  // Frozen time: only a follow already running can finish the tail; the budget never fires.
  vi.useFakeTimers();
  try {
    const seen: string[] = [];
    for await (const event of backfillThenFollow(tail(), follow())) {
      seen.push(event.type === "source_error" ? event.message : event.type);
    }
    expect(seen).toEqual(["tail", "live", "during tail", "after"]);
  } finally { vi.useRealTimers(); }
});

it("holds at most a bounded backlog while the tail loads, and loses none of it", async () => {
  vi.useFakeTimers();
  try {
    let pulled = 0;
    const hung: AsyncIterable<LogEvent> = { [Symbol.asyncIterator]() { return { next: () => new Promise<IteratorResult<LogEvent>>(() => {}) }; } };
    async function* flood(): AsyncIterable<LogEvent> {
      for (;;) {
        pulled++;
        yield { type: "source_error", machineId: "m", containerId: "c", message: String(pulled) };
      }
    }
    const events = backfillThenFollow(hung, flood())[Symbol.asyncIterator]();
    const first = events.next();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(pulled).toBeLessThanOrEqual(1_001);
    await vi.advanceTimersByTimeAsync(2_000);
    expect(await first).toMatchObject({ value: { type: "live" } });
    expect(await events.next()).toMatchObject({ value: { message: "1" } });
    await events.return?.(undefined);
  } finally { vi.useRealTimers(); }
});

it("stops waiting on a hung tail and follows anyway", async () => {
  vi.useFakeTimers();
  try {
    async function* hung(): AsyncIterable<LogEvent> {
      yield { type: "source_error", machineId: "m", containerId: "c", message: "fast" };
      await new Promise(() => {});
    }
    async function* follow(): AsyncIterable<LogEvent> {
      yield { type: "source_error", machineId: "m", containerId: "c", message: "follow" };
    }
    const events = backfillThenFollow(hung(), follow())[Symbol.asyncIterator]();
    expect(await events.next()).toMatchObject({ value: { message: "fast" } });
    const live = events.next();
    await vi.advanceTimersByTimeAsync(3_000);
    expect(await live).toMatchObject({ value: { type: "live" } });
    expect(await events.next()).toMatchObject({ value: { message: "follow" } });
  } finally { vi.useRealTimers(); }
});

it("streams every record in order and closes its runtime scope", async () => {
  let closed = 0;
  async function* events() {
    yield { type: "live" as const };
    for (let index = 0; index < 3; index++) yield { type: "source_error" as const, machineId: "m", containerId: "c", message: String(index) };
  }
  const response = containerLogResponse(new Request("http://localhost/logs"), events(), async () => { closed++; });
  const body = await response.text();
  expect(body.startsWith("retry: 3000\n\nevent: live\n")).toBe(true);
  expect(body.match(/event: log/g)).toHaveLength(3);
  expect(body.indexOf('"message":"0"')).toBeLessThan(body.indexOf('"message":"2"'));
  expect(closed).toBe(1);
});

it("ends the stream on an upstream failure, for the browser to retry, and releases the runtime", async () => {
  let closed = false;
  const events = { [Symbol.asyncIterator]() { return { next: () => Promise.reject(new Error("private detail")) }; } };
  const response = containerLogResponse(new Request("http://localhost/logs"), events, async () => { closed = true; });
  expect(await response.text()).toBe("retry: 3000\n\n");
  expect(closed).toBe(true);
});

it("releases the runtime as soon as the viewer cancels an idle stream", async () => {
  let closed = false;
  const events = { [Symbol.asyncIterator]() { return { next: () => new Promise<IteratorResult<never>>(() => {}) }; } };
  const response = containerLogResponse(new Request("http://localhost/logs"), events, async () => { closed = true; });
  expect(response.headers.get("Cache-Control")).toBe("private, no-store, no-transform");
  await response.body?.cancel();
  expect(closed).toBe(true);
});

it("releases the runtime once when the viewer cancels a running stream", async () => {
  let closed = 0;
  const events = { [Symbol.asyncIterator]() { return { next: () => new Promise<IteratorResult<never>>(() => {}) }; } };
  const response = containerLogResponse(new Request("http://localhost/logs"), events, async () => { closed++; });
  const reader = response.body?.getReader();
  // Let the stream start pulling, so both the cancel and the stream's own end release it.
  await new Promise((resolve) => setTimeout(resolve, 10));
  await reader?.cancel();
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(closed).toBe(1);
});

it("releases the runtime once and ends the stream when the request aborts during a hung read", async () => {
  let closed = 0;
  const events = { [Symbol.asyncIterator]() { return { next: () => new Promise<IteratorResult<never>>(() => {}) }; } };
  const request = new AbortController();
  const response = containerLogResponse(new Request("http://localhost/logs", { signal: request.signal }), events, async () => { closed++; });
  const reader = response.body?.getReader();
  await reader?.read(); // retry
  const read = reader?.read();
  await new Promise((resolve) => setTimeout(resolve, 10));
  request.abort();
  expect(await read).toMatchObject({ done: true });
  expect(closed).toBe(1);
});

it("releases the runtime when the request aborts while the viewer isn't reading", async () => {
  let closed = 0;
  async function* events() {
    yield { type: "source_error" as const, machineId: "m", containerId: "c", message: "queued" };
    await new Promise(() => {});
  }
  const request = new AbortController();
  containerLogResponse(new Request("http://localhost/logs", { signal: request.signal }), events(), async () => { closed++; });
  // The first record fills the stream's queue, so nothing is pulling when the request aborts.
  await new Promise((resolve) => setTimeout(resolve, 10));
  request.abort();
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(closed).toBe(1);
});
