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
