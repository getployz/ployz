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
  return { machine_id: "machine", container_id: id, namespace: "env", labels: { "cloud.ployz.service.id": service, "ployz.deployment.id": "deploy" }, resolved_spec: { name: "api" }, kind: "service_container", runtime: { state: "running" } };
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
  const output = logs(t, { filter: { namespace: "env", serviceId: "service" }, signal: abort.signal });
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
function store(pages) {
  const inputs = [];
  return { inputs,
    async *watch() { yield { machines: [{ machine: { id: "m1", name: "fsn-1" } }, { machine: { id: "m2", name: "fsn-2" } }], containers: [] }; },
    async history(input) {
      inputs.push(input);
      const page = pages(input);
      if (page instanceof Error) throw page;
      const rows = [...page];
      return { async next() { return rows.shift() ?? null; }, cancel() {} };
    },
  };
}
const described = (id, kind = "service") => ({ row: "container", container_id: id, namespace: "env", service: "api", deployment: "old", replica: "api-1", kind });
const line = (id, time, message = "output", level = "info") => ({ row: "line", container_id: id, timestamp_nanos: String(time), stream: "stdout", level, message });
const end = next => ({ row: "end", next });
test("history reads a removed deployment from every Server's store and pages past the newest of their oldest rows", async () => {
  const t = store(input => {
    if (input.machine_id === "m1") return input.cursor === "m1-page-2" ? [described("a"), line("a", 90), end(null)]
      : [described("a"), line("a", 120, "{\"level\":\"error\"}", "error"), line("a", 100), line("a", 100), { row: "exit", container_id: "a", timestamp_nanos: "130", exit_code: 137, oom_killed: true }, end("m1-page-2")];
    if (input.until_nanos === "100") return [described("b"), line("b", 70), end(null)];
    return [described("b"), line("b", 110), { row: "gap", container_id: "b", from_nanos: "80", to_nanos: "95", reason: "not_captured" }, line("b", 60), end("m2-page-2")];
  });
  const first = await history(t, { filter: { namespace: "env", deploymentId: "old" }, limit: 4 });
  assert.deepEqual(t.inputs.map(({ machine_id, namespace, deployment, direction, limit }) => ({ machine_id, namespace, deployment, direction, limit })),
    ["m1", "m2"].map(machine_id => ({ machine_id, namespace: "env", deployment: "old", direction: "backward", limit: 4 })));
  assert.deepEqual(first.records.map(record => [record.source.machine_name, record.timestamp_nanos, record.level]),
    [["fsn-1", "120", "error"], ["fsn-2", "110", "info"], ["fsn-1", "100", "info"], ["fsn-1", "100", "info"]]);
  assert.equal(new Set(first.records.map(record => record.id)).size, 4);
  assert.deepEqual(first.exits, [{ machineId: "m1", machineName: "fsn-1", containerId: "a", serviceName: "api", timestampNanos: "130", exitCode: 137, oomKilled: true }]);
  // fsn-2 went further back than fsn-1; its rows older than fsn-1's oldest wait for the next page.
  assert.deepEqual(first.gaps, []);
  assert.deepEqual(first.failures, []);
  t.inputs.length = 0;
  const second = await history(t, { filter: { namespace: "env", deploymentId: "old" }, cursor: first.cursor, limit: 4 });
  assert.deepEqual(t.inputs.map(input => [input.machine_id, input.cursor, input.until_nanos]), [["m1", "m1-page-2", null], ["m2", null, "100"]]);
  assert.deepEqual(second.records.map(record => record.timestamp_nanos), ["90", "70"]);
  assert.equal(second.cursor, null);
  await assert.rejects(history(t, { filter: { namespace: "env" }, cursor: "not ours" }), /history page/);
});
test("a first page asked to start before a time reads every Server from there", async () => {
  const t = store(() => [described("a"), line("a", 40), end(null)]);
  const page = await history(t, { filter: { namespace: "env" }, before: "50" });
  assert.deepEqual(t.inputs.map(input => [input.machine_id, input.cursor, input.until_nanos]), [["m1", null, "50"], ["m2", null, "50"]]);
  assert.deepEqual(page.records.map(record => record.timestamp_nanos), ["40", "40"]);
});
test("a Server whose page ended early with nothing in it carries on from its own cursor", async () => {
  const t = store(input => input.machine_id === "m1"
    ? (input.cursor === "m1-page-2" ? [described("a"), line("a", 95), end(null)] : [end("m1-page-2")])
    : [described("b"), line("b", 110), line("b", 100), end("m2-page-2")]);
  const first = await history(t, { filter: { namespace: "env" }, limit: 4 });
  assert.deepEqual(first.records.map(record => record.timestamp_nanos), ["110", "100"]);
  t.inputs.length = 0;
  await history(t, { filter: { namespace: "env" }, cursor: first.cursor, limit: 4 });
  assert.deepEqual(t.inputs.map(input => [input.machine_id, input.cursor, input.until_nanos]), [["m1", "m1-page-2", null], ["m2", "m2-page-2", null]]);
});
test("a Server whose store fails is named, the others' lines still arrive, and the next page asks it again", async () => {
  const t = store(input => input.machine_id === "m2" ? new Error("the Server's Log Store stopped answering") : [described("a"), line("a", 5), end(null)]);
  const page = await history(t, { filter: { namespace: "env" } });
  assert.deepEqual(page.records.map(record => record.source.machine_id), ["m1"]);
  assert.deepEqual(page.failures, [{ machineId: "m2", machineName: "fsn-2", message: "the Server's Log Store stopped answering" }]);
  t.inputs.length = 0;
  await history(t, { filter: { namespace: "env" }, cursor: page.cursor });
  assert.deepEqual(t.inputs.map(input => input.machine_id), ["m2"]);
});
test("a stored line has the same id however it was read, and a group a page end cuts keeps distinct ids", async () => {
  const rows = [line("a", 100, "second"), line("a", 100, "first"), line("a", 90)];
  const t = store(input => input.machine_id === "m2" ? [end(null)]
    : input.cursor === "m1-page-2" ? [described("a"), rows[1], rows[2], end(null)]
    : input.until_nanos === null ? [described("a"), rows[0], end("m1-page-2")]
    : [described("a"), ...rows, end(null)]);
  const newest = await history(t, { filter: { namespace: "env" }, limit: 1 });
  const older = await history(t, { filter: { namespace: "env" }, cursor: newest.cursor, limit: 2 });
  const before = await history(t, { filter: { namespace: "env" }, before: "101", limit: 3 });
  const ids = [...newest.records, ...older.records].map(record => record.id);
  assert.equal(new Set(ids).size, 3);
  assert.deepEqual(before.records.map(record => record.id).sort(), [...ids].sort());
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


test("identical stored lines keep distinct replayable IDs across cursor pages", async () => {
  let failed = false;
  const t = store(input => input.machine_id === "m2" ? [end(null)]
    : failed ? new Error("temporarily unavailable")
    : input.limit === 3 ? [described("a"), line("a", 100), line("a", 100), line("a", 100), end(null)]
    : [described("a"), line("a", 100), end(input.cursor === null ? "second" : input.cursor === "second" ? "third" : null)]);
  const first = await history(t, { filter: { namespace: "env" }, limit: 1 });
  failed = true;
  const failure = await history(t, { filter: { namespace: "env" }, cursor: first.cursor, limit: 1 });
  assert.equal(failure.cursor, first.cursor);
  failed = false;
  const second = await history(t, { filter: { namespace: "env" }, cursor: failure.cursor, limit: 1 });
  const replay = await history(t, { filter: { namespace: "env" }, cursor: first.cursor, limit: 1 });
  const third = await history(t, { filter: { namespace: "env" }, cursor: second.cursor, limit: 1 });
  assert.deepEqual(replay, second);
  assert.equal(new Set([...first.records, ...second.records, ...third.records].map(record => record.id)).size, 3);
  assert.equal(third.cursor, null);
  const combined = await history(t, { filter: { namespace: "env" }, limit: 3 });
  assert.deepEqual([...first.records, ...second.records, ...third.records].map(record => record.id).sort(), combined.records.map(record => record.id).sort());
  assert.ok(second.cursor.length < 256, "a duplicate group needs bounded cursor metadata");
});

test("a stalled history reader is cancelled and returns a named failure with healthy rows", async context => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const t = store(() => [described("a"), line("a", 100), end(null)]);
  const healthy = t.history;
  let cancelled = false;
  t.history = input => input.machine_id === "m1" ? healthy(input)
    : Promise.resolve({ next: () => new Promise((_resolve, reject) => setTimeout(() => reject(new Error("the Server's Log Store deadline exceeded")), 10_000)), cancel() { cancelled = true; } });
  let page;
  const finished = history(t, { filter: { namespace: "env" } }).then(value => { page = value; });
  await tick();
  context.mock.timers.tick(10_000);
  await tick();
  assert.ok(page, "a stalled Server must not prevent a partial page after the row deadline");
  await finished;
  assert.equal(cancelled, true);
  assert.deepEqual(page.records.map(record => record.source.machine_id), ["m1"]);
  assert.deepEqual(page.failures.map(failure => failure.machineId), ["m2"]);
  assert.match(page.failures[0].message, /deadline|timed out/);
  assert.ok(page.cursor);
});


test("duplicate IDs survive a fanout horizon while time-bound Servers restart their ordinals", async () => {
  const t = store(input => {
    const rows = input.machine_id === "m1" ? [line("a", 100), line("a", 100), line("a", 100), line("a", 90)]
      : [line("b", 101), line("b", 95), line("b", 95), line("b", 80)];
    const from = Number(input.cursor ?? 0);
    const selected = rows.slice(from).filter(row => input.until_nanos === null || BigInt(row.timestamp_nanos) < BigInt(input.until_nanos));
    const page = selected.slice(0, input.limit);
    return [described(input.machine_id === "m1" ? "a" : "b"), ...page, end(page.length < selected.length ? String(rows.indexOf(page.at(-1)) + 1) : null)];
  });
  const first = await history(t, { filter: { namespace: "env" }, limit: 2 });
  const pages = [...first.records];
  let cursor = first.cursor;
  for (let read = 0; cursor && read < 10; read++) {
    const page = await history(t, { filter: { namespace: "env" }, cursor, limit: 2 });
    const replay = await history(t, { filter: { namespace: "env" }, cursor, limit: 2 });
    assert.deepEqual(replay, page);
    pages.push(...page.records);
    cursor = page.cursor;
  }
  assert.equal(cursor, null);
  const whole = await history(t, { filter: { namespace: "env" }, limit: 20 });
  assert.equal(new Set(pages.map(record => record.id)).size, 8);
  assert.deepEqual(pages.map(record => record.id).sort(), whole.records.map(record => record.id).sort());
  assert.ok(t.inputs.some(input => input.until_nanos !== null), "the fanout must exercise time-bound continuation");
});
