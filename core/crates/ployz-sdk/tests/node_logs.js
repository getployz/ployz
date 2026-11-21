"use strict";
const { test } = require("node:test");
const assert = require("node:assert/strict");
const { logs } = require("../runtime-logs.js");
// Stands in for the request's SealedNames handle; the fakes below only check it arrives.
const sealed = { handle: "sealed names" };
function queue() {
  const values = []; let waiting;
  return {
    push(value) { if (waiting) { const resolve = waiting; waiting = null; resolve(value); } else values.push(value); },
    next() { return values.length ? Promise.resolve(values.shift()) : new Promise(resolve => { waiting = resolve; }); },
    cancel() { this.push(null); },
  };
}
function container(id, service = "service") {
  return { machine_id: "machine", container_id: id, namespace: "env", labels: { "cloud.ployz.service.id": service, "ployz.deployment.id": "deploy" }, resolved_spec: { name: "api" }, kind: "service_container", runtime: { state: "running" } };
}
function row(id, time, message = "output") {
  return { source: { machine_id: "machine", machine_name: "server", origin: { origin: "service", container_id: id, service_name: "api" } }, timestamp_nanos: String(time), channel: "stdout", text: "shown", level: "info", message };
}
function transport() {
  const frames = queue(); const readers = new Map(); const requests = [];
  return { frames, readers, requests,
    watch({ signal }) {
      signal?.addEventListener("abort", () => frames.push(null), { once: true });
      return { [Symbol.asyncIterator]() { return this; }, async next() { const value = await frames.next(); return value ? { value, done: false } : { done: true }; }, async return() { return { done: true }; } };
    },
    async open(input, names) { assert.equal(names, sealed); requests.push(input); const reader = queue(); readers.set(input.container_id, reader); return reader; },
  };
}
const tick = () => new Promise(resolve => setImmediate(resolve));
test("discovers late containers, keeps duplicates, replays restarts and cancels readers", async () => {
  const t = transport(); const abort = new AbortController();
  const output = logs(t, { sealed, filter: { namespace: "env", serviceId: "service" }, signal: abort.signal });
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
test("one failed source does not interrupt other containers", async () => {
  const t = transport(); const open = t.open;
  t.open = (input, names) => input.container_id === "bad" ? Promise.reject(new Error("unavailable")) : open(input, names);
  t.frames.push({ containers: [container("bad"), container("good")] });
  const output = logs(t, { sealed, follow: false });
  assert.equal((await output.next()).value.type, "source_error");
  t.readers.get("good").push(row("good", 1)); t.readers.get("good").push(null);
  assert.equal((await output.next()).value.type, "record");
  assert.equal((await output.next()).done, true);
});

test("a failed source whose container leaves a complete observation is gone", async () => {
  const t = transport(); const open = t.open; const abort = new AbortController();
  t.open = (input, names) => input.container_id === "bad" ? Promise.reject(new Error("unavailable")) : open(input, names);
  const output = logs(t, { sealed, signal: abort.signal });
  try {
    t.frames.push({ containers: [container("bad"), container("good")] });
    assert.deepEqual((await output.next()).value, { type: "source_error", machineId: "machine", containerId: "bad", message: "unavailable" });
    t.frames.push({ containers: [container("good")], incomplete_ids: { machines: ["machine"] } });
    t.frames.push({ containers: [container("good")] });
    assert.deepEqual((await output.next()).value, { type: "source_gone", machineId: "machine", containerId: "bad" });
    t.readers.get("good").push(row("good", 1));
    assert.equal((await output.next()).value.type, "record");
  } finally {
    const end = output.next(); abort.abort(); await end;
  }
});

test("a later running observation follows after the immediate EOF handoff ends", async () => {
  const t = transport();
  const abort = new AbortController();
  const output = logs(t, { sealed, signal: abort.signal });
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
  const output = logs(t, { sealed, signal: abort.signal });
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
  const output = logs(t, { sealed, signal: abort.signal });
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


const lacking = () => Object.assign(new Error("ployz.containers.logs.v1 is not served"), { code: "unsupported", details: { capability: "ployz.containers.logs.v1", target: "m2" } });
test("a source that refuses to open keeps the code and capability", async () => {
  const t = transport(); const open = t.open;
  t.open = (input, names) => input.container_id === "bad" ? Promise.reject(lacking()) : open(input, names);
  t.frames.push({ containers: [container("bad")] });
  const output = logs(t, { sealed, follow: false });
  assert.deepEqual((await output.next()).value, { type: "source_error", machineId: "machine", containerId: "bad", message: "ployz.containers.logs.v1 is not served", code: "unsupported", capability: "ployz.containers.logs.v1" });
  assert.equal((await output.next()).done, true);
});
test("a source that fails mid-stream keeps the code and capability", async () => {
  const t = transport();
  t.open = async () => ({ next: () => Promise.reject(lacking()), cancel() {} });
  t.frames.push({ containers: [container("bad")] });
  const output = logs(t, { sealed, follow: false });
  assert.deepEqual((await output.next()).value, { type: "source_error", machineId: "machine", containerId: "bad", message: "ployz.containers.logs.v1 is not served", code: "unsupported", capability: "ployz.containers.logs.v1" });
});

test("logs refuses to read without the request's Sealed names", async () => {
  await assert.rejects(logs(transport(), {}).next(), /sealed is required/);
});

test("a value split across a clean end of stream and the reopen shows the marker, never the value", async () => {
  const value = "73519086427351908642";
  const t = transport(); const abort = new AbortController();
  // Each reader answers as the scrubbing reader does: a clean end shows the held prefix as the marker, and the
  // reopen holds the redelivered prefix until the rest of the value arrives. Without the names it shows raw text.
  const said = (names, raw, scrubbed) => names === sealed ? scrubbed : raw;
  const replies = [
    names => [row("a", 100, said(names, `key=${value.slice(0, 10)}`, "key=[secret TOKEN]")), null],
    names => [row("a", 101, said(names, `key=${value}\n`, "key=[secret TOKEN]\n"))],
  ];
  t.open = async (input, names) => {
    t.requests.push(input);
    const reader = queue();
    for (const reply of replies.shift()(names)) reader.push(reply);
    return reader;
  };
  const output = logs(t, { sealed, signal: abort.signal });
  try {
    t.frames.push({ containers: [container("a")] });
    const shown = [(await output.next()).value.record, (await output.next()).value.record];
    assert.deepEqual(t.requests.map(input => input.since_unix_seconds), [null, 0]);
    const joined = shown.map(record => record.message).join("");
    assert.equal(joined, "key=[secret TOKEN]key=[secret TOKEN]\n");
    assert.doesNotMatch(joined, /[0-9]/);
  } finally {
    const end = output.next(); abort.abort(); await end;
  }
});

test("a line the reader couldn't check reaches the viewer as a hidden record", async () => {
  const t = transport(); const abort = new AbortController();
  const output = logs(t, { sealed, signal: abort.signal });
  try {
    t.frames.push({ containers: [container("a")] });
    const next = output.next(); await tick();
    const { message: _message, level: _level, ...hidden } = row("a", 100);
    t.readers.get("a").push({ ...hidden, text: "hidden" });
    const record = (await next).value.record;
    assert.equal(record.text, "hidden");
    assert.equal(record.message, undefined);
  } finally {
    const end = output.next(); abort.abort(); await end;
  }
});
