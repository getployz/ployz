import { afterEach, expect, it, vi } from "vitest";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";

afterEach(() => {
  vi.useRealTimers();
});

it("frames an event", () => {
  expect(sseEvent({ event: "changes", data: { a: 1 } })).toBe('event: changes\ndata: {"a":1}\n\n');
});

it("pings while no event arrives and delivers the pending event on a later pull", async () => {
  vi.useFakeTimers();
  async function* events() {
    await new Promise((resolve) => setTimeout(resolve, 20_000));
    yield sseEvent({ event: "late", data: {} });
  }
  const response = eventStreamResponse(new AbortController().signal, events, { heartbeatMs: 15_000, retryMs: 1000 });
  if (!response.body) throw new Error("The event stream has no body");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const read = async () => decoder.decode((await reader.read()).value);

  expect(await read()).toBe("retry: 1000\n\n");
  const ping = read();
  await vi.advanceTimersByTimeAsync(15_000);
  expect(await ping).toBe(": ping\n\n");
  const late = read();
  await vi.advanceTimersByTimeAsync(5_000);
  expect(await late).toBe("event: late\ndata: {}\n\n");
  await reader.cancel();
});

it("aborts the events' signal when the client cancels", async () => {
  let signal: AbortSignal | undefined;
  async function* events() {
    yield ": first\n\n";
  }
  const response = eventStreamResponse(new AbortController().signal, (received) => {
    signal = received;
    return events();
  }, { heartbeatMs: 15_000 });
  await response.body?.cancel();
  expect(signal?.aborted).toBe(true);
});
