import { expect, it, vi } from "vitest";
import type { LogEvent, LogMetadata } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { backfillThenFollow, containerLogResponse, whileLogsShown } from "./container-log-events.server";

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
    for (let index = 0; index < 3; index++) yield { type: "source_gone" as const, machineId: "m", containerId: String(index) };
  }
  const response = containerLogResponse(new Request("http://localhost/logs"), events(), async () => { closed++; });
  const body = await response.text();
  expect(body.startsWith("retry: 3000\n\nevent: live\n")).toBe(true);
  expect(body.match(/event: log/g)).toHaveLength(3);
  expect(body.indexOf('"containerId":"0"')).toBeLessThan(body.indexOf('"containerId":"2"'));
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
  let pulled = false;
  const events = { [Symbol.asyncIterator]() { return { next: () => { pulled = true; return new Promise<IteratorResult<never>>(() => {}); } }; } };
  const response = containerLogResponse(new Request("http://localhost/logs"), events, async () => { closed++; });
  const reader = response.body?.getReader();
  await reader?.read();
  const read = reader?.read();
  await vi.waitFor(() => expect(pulled).toBe(true));
  await reader?.cancel();
  expect(await read).toMatchObject({ done: true });
  expect(closed).toBe(1);
});

it("releases the runtime once and ends the stream when the request aborts during a hung read", async () => {
  let closed = 0;
  let pulled = false;
  const events = { [Symbol.asyncIterator]() { return { next: () => { pulled = true; return new Promise<IteratorResult<never>>(() => {}); } }; } };
  const request = new AbortController();
  const response = containerLogResponse(new Request("http://localhost/logs", { signal: request.signal }), events, async () => { closed++; });
  const reader = response.body?.getReader();
  await reader?.read();
  const read = reader?.read();
  await vi.waitFor(() => expect(pulled).toBe(true));
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
  request.abort();
  expect(closed).toBe(1);
});

it("closes support's stream with hidden once the Organization hides its logs, even while lines keep coming", async () => {
  const record = (message: string): LogEvent => ({ type: "source_error", machineId: "m", containerId: "c", message });
  let released = false;
  vi.useFakeTimers();
  try {
    async function* flood(): AsyncIterable<LogEvent> {
      try {
        for (let line = 0; line < 1_000; line++) {
          yield record(`line ${line}`);
          vi.setSystemTime(Date.now() + 1);
        }
      } finally { released = true; }
    }
    const answers = [true, false];
    const seen: string[] = [];
    for await (const event of whileLogsShown(flood(), async () => answers.shift() ?? false, 10)) seen.push(event.type);
    expect(seen.at(-1)).toBe("hidden");
    expect(seen.filter(type => type === "hidden")).toHaveLength(1);
    expect(seen.length).toBeGreaterThan(2);
    await vi.waitFor(() => expect(released).toBe(true));
  } finally { vi.useRealTimers(); }
});

it("ends support's stream without hidden when what it may see can't be read, so the reconnect asks again", async () => {
  async function* quiet(): AsyncIterable<LogEvent> { await new Promise(() => {}); }
  const seen: string[] = [];
  for await (const event of whileLogsShown(quiet(), () => Promise.reject(new Error("database down")), 5)) seen.push(event.type);
  expect(seen).toEqual([]);
});

it("sends a line it couldn't check as a hidden gap, and a Machine's error as fixed copy, never their text", async () => {
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    const source = asTestDouble<LogMetadata>()({ machine_id: "m", machine_name: "fsn-1", origin: { origin: "service" as const, service_id: "s", service_name: "api", container_id: "c", hook: null } });
    async function* events(): AsyncIterable<LogEvent> {
      yield { type: "record", record: { id: "m/c/100/000000000000", source, timestamp_nanos: "100", channel: "stdout", text: "hidden" } };
      yield { type: "source_error", machineId: "m", containerId: "c", message: "boom: TOKEN=73519086427351908642" };
    }
    const body = await containerLogResponse(new Request("http://localhost/logs"), events(), async () => {}).text();
    const logs = body.split("\n").filter(line => line.startsWith("data: ")).map(line => JSON.parse(line.slice(6)));
    expect(logs).toEqual([
      { type: "record", record: { kind: "gap", id: "m/c/100/000000000000", timestamp: "100", until: "100", machineId: "m", machineName: "fsn-1", containerId: "c", serviceName: "api", reason: "hidden" } },
      { type: "source_error", machineId: "m", containerId: "c", message: "Container logs are unavailable." },
    ]);
    expect(body).not.toContain("7351908642");
    expect(error).toHaveBeenCalledWith("A container log source failed.", expect.objectContaining({ message: "boom: TOKEN=73519086427351908642" }));
  } finally { error.mockRestore(); }
});
