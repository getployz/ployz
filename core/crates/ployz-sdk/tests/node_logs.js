"use strict";
const { test } = require("node:test");
const assert = require("node:assert/strict");
const { logs, history } = require("../runtime-logs.js");
function queue() {
  const values = []; let waiting;
  return {
    push(value) { if (waiting) { const resolve = waiting; waiting = null; resolve(value); } else values.push(value); },
    next() { return values.length ? Promise.resolve(values.shift()) : new Promise(resolve => { waiting = resolve; }); },
    cancel() { this.push(null); },
  };
}
function container(id, service = "service") {
  return { machine_id: "machine", container_id: id, project_name: "env", labels: { "cloud.ployz.service.id": service, "ployz.deployment.id": "deploy" }, resolved_spec: { name: "api" }, kind: "service_container", runtime: { state: "running" } };
}
function row(id, time, message = "output") {
  return { source: { machine_id: "machine", machine_name: "server", origin: { origin: "service", container_id: id, service_name: "api" } }, timestamp_nanos: String(time), channel: "stdout", message };
}
function transport() {
  const frames = queue(); const readers = new Map(); const requests = [];
  return { frames, readers, requests,
    watch({ signal }) {
      signal?.addEventListener("abort", () => frames.push(null), { once: true });
      return { [Symbol.asyncIterator]() { return this; }, async next() { const value = await frames.next(); return value ? { value, done: false } : { done: true }; }, async return() { return { done: true }; } };
    },
    async open(input) { requests.push(input); const reader = queue(); readers.set(input.container_id, reader); return reader; },
  };
}
const tick = () => new Promise(resolve => setImmediate(resolve));
test("discovers late containers, keeps duplicates, replays restarts and cancels readers", async () => {
  const t = transport(); const abort = new AbortController();
  const output = logs(t, { filter: { projectName: "env", serviceId: "service" }, signal: abort.signal });
  t.frames.push({ containers: [container("a"), container("excluded", "other")] });
  const first = output.next(); await tick();
  t.readers.get("a").push(row("a", 100));
  assert.equal((await first).value.record.message, "output");
  t.readers.get("a").push(row("a", 100));
  const duplicate = (await output.next()).value.record;
  assert.match(duplicate.id, /\/000000000001$/);
  t.readers.get("a").push(null);
  const next = output.next(); await tick();
  t.frames.push({ containers: [container("a"), container("b")] }); await tick();
  assert.equal(t.requests.find(input => input.container_id === "excluded"), undefined);
  assert.equal(t.requests.filter(input => input.container_id === "a")[1].since_unix_seconds, 0);
  t.readers.get("a").push(row("a", 100));
  t.readers.get("a").push(row("a", 100));
  t.readers.get("a").push(row("a", 101, "after restart"));
  assert.equal((await next).value.record.message, "after restart");
  const late = output.next(); t.readers.get("b").push(row("b", 110));
  assert.equal((await late).value.record.source.origin.container_id, "b");
  const end = output.next(); abort.abort(); assert.equal((await end).done, true);
});
test("history uses each source's boundary and preserves equal messages", async () => {
  const inputs = [];
  const t = {
    async *watch() { yield { containers: [container("a"), container("b")] }; },
    async open(input) {
      inputs.push(input);
      const values = [row(input.container_id, Number(input.before_nanos) - 1), row(input.container_id, Number(input.before_nanos) - 1)];
      return { async next() { return values.shift() ?? null; }, cancel() {} };
    },
  };
  const page = await history(t, { before: { "machine/a": "100", "machine/b": "500" }, limit: 200 });
  assert.deepEqual(inputs.map(input => input.before_nanos), ["100", "500"]);
  assert.equal(page.records.length, 4);
  assert.equal(new Set(page.records.map(record => record.id)).size, 4);
  await assert.rejects(history(t, { before: { "machine/a": "invalid" } }), /nanosecond/);
});
test("one failed source does not interrupt other containers", async () => {
  const t = transport(); const open = t.open;
  t.open = input => input.container_id === "bad" ? Promise.reject(new Error("unavailable")) : open(input);
  t.frames.push({ containers: [container("bad"), container("good")] });
  const output = logs(t, { follow: false });
  assert.equal((await output.next()).value.type, "source_error");
  t.readers.get("good").push(row("good", 1)); t.readers.get("good").push(null);
  assert.equal((await output.next()).value.type, "record");
  assert.equal((await output.next()).done, true);
});

test("a later running observation follows after the immediate EOF handoff ends", async () => {
  const t = transport();
  const abort = new AbortController();
  const output = logs(t, { signal: abort.signal });
  try {
    t.frames.push({ containers: [container("a")] });
    const first = output.next();
    await tick();
    t.readers.get("a").push(row("a", 1));
    await first;
    t.readers.get("a").push(null);
    const resumed = output.next();
    await tick();
    assert.equal(t.requests.length, 2);
    t.readers.get("a").push(null);
    await tick();
    await tick();
    assert.equal(t.requests.length, 2);
    t.frames.push({ containers: [container("a")] });
    await tick();
    assert.equal(t.requests.length, 3);
    t.readers.get("a").push(null);
    await tick();
    assert.equal(t.requests.length, 3);
    t.frames.push({ containers: [container("a")] });
    await tick();
    assert.equal(t.requests.length, 4);
    t.readers.get("a").push(row("a", 4, "resumed"));
    assert.equal((await resumed).value.record.message, "resumed");
  } finally {
    const end = output.next();
    abort.abort();
    await end;
  }
});
test("clean EOF reattaches when the running observation already arrived", async () => {
  const t = transport(); const abort = new AbortController();
  const output = logs(t, { signal: abort.signal });
  t.frames.push({ containers: [container("a")] });
  const first = output.next(); await tick(); t.readers.get("a").push(row("a", 1)); await first;
  t.readers.get("a").push(null);
  const resumed = output.next(); await tick();
  assert.equal(t.requests.length, 2);
  t.readers.get("a").push(row("a", 2));
  assert.equal((await resumed).value.record.timestamp_nanos, "2");
  const end = output.next(); abort.abort(); await end;
});

test("fresh running observations resume after two clean EOFs without spinning", async () => {
  const t = transport(); const abort = new AbortController();
  const output = logs(t, { signal: abort.signal });
  t.frames.push({ containers: [container("a")] });
  const next = output.next(); await tick();
  t.readers.get("a").push(null); await tick();
  t.readers.get("a").push(null); await tick();
  assert.equal(t.requests.length, 2);
  await tick(); assert.equal(t.requests.length, 2);
  t.frames.push({ containers: [container("a")] }); await tick();
  assert.equal(t.requests.length, 3);
  t.readers.get("a").push(row("a", 3));
  assert.equal((await next).value.record.timestamp_nanos, "3");
  const end = output.next(); abort.abort(); await end;
});
