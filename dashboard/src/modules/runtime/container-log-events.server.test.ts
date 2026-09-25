import { expect, it } from "vitest";
import { containerLogResponse } from "./container-log-events.server";

it("streams every record in order and closes its runtime scope", async () => {
  let closed = 0;
  async function* events() {
    for (let index = 0; index < 3; index++) yield { type: "source_error" as const, machineId: "m", containerId: "c", message: String(index) };
  }
  const response = containerLogResponse(new Request("http://localhost/logs"), events(), async () => { closed++; });
  const body = await response.text();
  expect(body.match(/event: log/g)).toHaveLength(3);
  expect(body.indexOf('"message":"0"')).toBeLessThan(body.indexOf('"message":"2"'));
  expect(closed).toBe(1);
});

it("reports a stream failure and releases the runtime", async () => {
  let closed = false;
  const events = { [Symbol.asyncIterator]() { return { next: () => Promise.reject(new Error("private detail")) }; } };
  const response = containerLogResponse(new Request("http://localhost/logs"), events, async () => { closed = true; });
  expect(await response.text()).toBe('event: unavailable\ndata: {}\n\n');
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
