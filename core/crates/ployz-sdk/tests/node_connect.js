"use strict";
const { test, after } = require("node:test");
const assert = require("node:assert/strict");
const { getEventListeners } = require("node:events");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const Module = require("node:module");

const pkg = path.join(__dirname, "..");
// Node caches the addon under its real path, so a symlinked temp dir (macOS /var) would miss the stub below.
const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-connect-")));
after(() => fs.rmSync(dir, { recursive: true, force: true }));
for (const file of ["index.js", "wrap.js", "runtime-logs.js"]) fs.copyFileSync(path.join(pkg, file), path.join(dir, file));
const addon = path.join(dir, "ployz-sdk.node");
fs.writeFileSync(addon, "");

// A native session whose close settles only when the test says so, and which records every request to stop it.
let session;
class NativeClient {
  constructor() { this.stops = 0; this.closes = []; }
  close() { this.stops += 1; return new Promise((resolve, reject) => this.closes.push({ resolve, reject })); }
  finishClose() { for (const close of this.closes.splice(0)) close.resolve(); }
  failClose(error) { for (const close of this.closes.splice(0)) close.reject(error); }
}
// Each connect's native session arrives through `connecting`, which a test may hold back.
const arriveAtOnce = async () => (session = new NativeClient());
let connecting = arriveAtOnce;
let diagnostic = null;
let cancelReasons = [];
let nativeCorrelation;
const native = new Module(addon);
native.loaded = true;
native.exports = {
  Client: NativeClient,
  ConfigStore: class {},
  SealedNames: class {},
  startConnections: (_connections, _sampled, correlation) => {
    nativeCorrelation = correlation;
    return { cancel: (timedOut) => cancelReasons.push(timedOut), wait: () => connecting(), dialTimings: () => diagnostic };
  },
};
require.cache[addon] = native;
const sdk = require(dir);

test("a cancelled native dial says no Server answered", async () => {
  const binding = require(path.join(pkg, "ployz-sdk.node"));
  const { RpcError, withRpcError } = require(path.join(pkg, "wrap.js"));
  const pending = binding.startConnections([{ tcp: "127.0.0.1:1" }]);
  pending.cancel();
  await assert.rejects(
    withRpcError(pending.wait()),
    error => error instanceof RpcError && error.code === "unavailable" && error.unanswered === true,
  );
});

const abortListeners = signal => getEventListeners(signal, "abort").length;
const tick = () => new Promise(resolve => setImmediate(resolve));

test("sampled dial reports only stage durations and enum values", async () => {
  const lines = [];
  diagnostic = {
    totalMs: 71, endpointBindMs: 17, pathKind: "relayed", outcome: "answered",
    capability: "ployz1:private-fixture", relayUrl: "https://secret-relay.example.com",
  };
  try {
    const client = await sdk.connect({
      connections: [{ management: "ployz1:private-fixture" }],
      onDialTiming: (timing) => lines.push(timing),
    });
    assert.equal(client.dialTiming.endpointBindMs, 17);
    const logged = JSON.stringify(lines);
    assert.match(logged, /endpointBindMs/);
    assert.doesNotMatch(logged, /private-fixture|secret-relay\.example\.com/);
    const closing = client.close();
    session.finishClose();
    await closing;
  } finally {
    diagnostic = null;
  }
});

test("a rejecting async diagnostic callback leaves the dial result and process unchanged", async () => {
  const unhandled = [];
  const record = reason => unhandled.push(reason);
  process.on("unhandledRejection", record);
  diagnostic = { totalMs: 12, outcome: "answered", pathKind: "unknown" };
  try {
    const client = await sdk.connect({
      connections: [{ tcp: "127.0.0.1:1" }],
      onDialTiming: async () => { throw new Error("diagnostic failed"); },
    });
    assert.equal(client.dialTiming.outcome, "answered");
    await tick();
    await tick();
    assert.deepEqual(unhandled, []);
    const closing = client.close();
    session.finishClose();
    await closing;
  } finally {
    diagnostic = null;
    process.off("unhandledRejection", record);
  }
});

test("a fast unsampled dial has no dialTiming property", async () => {
  const client = await sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }] });
  assert.equal(Object.hasOwn(client, "dialTiming"), false);
  const closing = client.close();
  session.finishClose();
  await closing;
});

test("a caller abort reason remains untouched", async () => {
  const abort = new AbortController();
  const reason = new Error("caller gave up");
  diagnostic = { totalMs: 12, outcome: "cancelled", pathKind: "unknown" };
  try {
    let arrive;
    connecting = () => new Promise(resolve => (arrive = resolve));
    const pending = sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }], signal: abort.signal });
    abort.abort(reason);
    arrive((session = new NativeClient()));
    await tick();
    session.finishClose();
    await assert.rejects(pending, error => error === reason);
    assert.equal(Object.hasOwn(reason, "dialTiming"), false);
  } finally {
    connecting = arriveAtOnce;
    diagnostic = null;
  }
});

test("a supplied correlation value reaches native unchanged", async () => {
  const correlation = "0011223344556677";
  const client = await sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }], dialCorrelation: correlation });
  assert.equal(nativeCorrelation, correlation);
  const closing = client.close();
  session.finishClose();
  await closing;
});
const activeTimeouts = () => process.getActiveResourcesInfo().filter(resource => resource === "Timeout").length;

test("an abort during close reaches the native session, and a second abort changes nothing", async () => {
  const abort = new AbortController();
  const client = await sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }], signal: abort.signal });
  const closing = client.close();
  await tick();
  assert.equal(session.stops, 1, "close asked the native session to stop");
  abort.abort();
  abort.abort();
  await tick();
  assert.equal(session.stops, 2, "the abort during close reached the native session");
  session.finishClose();
  await closing;
  assert.equal(abortListeners(abort.signal), 0, "a finished close lets go of the session");
});

test("an abort after close has finished does nothing, and the signal holds no listener", async () => {
  const abort = new AbortController();
  const client = await sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }], signal: abort.signal });
  assert.equal(abortListeners(abort.signal), 1, "an open session listens for its abort");
  const closing = client.close();
  await tick();
  assert.equal(abortListeners(abort.signal), 1, "a close in flight still listens for its abort");
  session.finishClose();
  await closing;
  assert.equal(abortListeners(abort.signal), 0, "a finished close lets go of the session");
  abort.abort();
  await tick();
  assert.equal(session.stops, 1, "an abort after close never reached the native session");
});

test("a native close that fails after an abort reaches the caller's close, and no rejection goes unhandled", async () => {
  const unhandled = [];
  const record = reason => unhandled.push(reason);
  process.on("unhandledRejection", record);
  try {
    const abort = new AbortController();
    const client = await sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }], signal: abort.signal });
    abort.abort();
    const closing = client.close();
    await tick();
    assert.equal(session.stops, 2, "the abort and the caller each asked the native session to stop");
    const failure = new Error("native close failed");
    session.failClose(failure);
    await assert.rejects(closing, failure, "the caller's close sees its own failure");
    await tick();
    await tick();
    assert.deepEqual(unhandled, [], "the abort's close left no unhandled rejection");
    assert.equal(abortListeners(abort.signal), 0, "a failed close still lets go of the session");
  } finally {
    process.off("unhandledRejection", record);
  }
});

async function stopWhileConnectingThenFailClose(signal, timeoutMs, stopConnecting, expected) {
  const unhandled = [];
  const record = reason => unhandled.push(reason);
  process.on("unhandledRejection", record);
  let arrive;
  connecting = () => new Promise(resolve => (arrive = resolve));
  cancelReasons = [];
  try {
    const timeouts = activeTimeouts();
    const connected = sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }], signal, timeoutMs });
    await stopConnecting();
    assert.equal(cancelReasons.at(-1), timeoutMs !== undefined, "the native dial records the deadline reason");
    arrive((session = new NativeClient()));
    await tick();
    assert.equal(session.stops, 1, "connect closed the session that arrived after it stopped");
    session.failClose(new Error("native close failed"));
    await assert.rejects(connected, expected);
    await tick();
    await tick();
    assert.deepEqual(unhandled, [], "the failed close left no unhandled rejection");
    assert.equal(abortListeners(signal), 0, "a failed close still lets go of the session");
    assert.equal(activeTimeouts(), timeouts, "connect left no timer behind");
  } finally {
    connecting = arriveAtOnce;
    process.off("unhandledRejection", record);
  }
}

test("an abort while connecting rejects with the abort reason even when the native close fails", async () => {
  const abort = new AbortController();
  const reason = new Error("caller gave up");
  await stopWhileConnectingThenFailClose(abort.signal, undefined, async () => abort.abort(reason), error => error === reason);
});

test("a frozen abort reason survives diagnostic attachment", async () => {
  const abort = new AbortController();
  const reason = Object.freeze(new Error("caller gave up"));
  diagnostic = { totalMs: 12, outcome: "cancelled", pathKind: "unknown" };
  try {
    await stopWhileConnectingThenFailClose(abort.signal, undefined, async () => abort.abort(reason), error => error === reason);
  } finally {
    diagnostic = null;
  }
});

test("a deadline while connecting rejects as exceeded even when the native close fails", async () => {
  const abort = new AbortController();
  await stopWhileConnectingThenFailClose(
    abort.signal,
    1,
    () => new Promise(resolve => setTimeout(resolve, 20)),
    { message: "session deadline exceeded" },
  );
});

async function stopWhileConnectingThenCloseHangs(signal, timeoutMs, stopConnecting, expected) {
  const unhandled = [];
  const record = reason => unhandled.push(reason);
  process.on("unhandledRejection", record);
  let arrive;
  connecting = () => new Promise(resolve => (arrive = resolve));
  try {
    const timeouts = activeTimeouts();
    const connected = sdk.connect({ connections: [{ tcp: "127.0.0.1:1" }], signal, timeoutMs });
    await stopConnecting();
    arrive((session = new NativeClient()));
    await tick();
    assert.equal(session.stops, 1, "connect closed the session that arrived after it stopped");
    let hold;
    const held = new Promise(resolve => (hold = setTimeout(resolve, 5_000, "held")));
    const outcome = await Promise.race([connected.then(() => "connected", () => "rejected"), held]);
    clearTimeout(hold);
    assert.equal(outcome, "rejected", "connect gave up on a native close that never settles");
    await assert.rejects(connected, expected);
    session.failClose(new Error("native close failed late"));
    await tick();
    await tick();
    assert.deepEqual(unhandled, [], "a close that fails after connect gave up left no unhandled rejection");
    assert.equal(abortListeners(signal), 0, "connect let go of the signal without the close");
    assert.equal(activeTimeouts(), timeouts, "connect left no timer behind");
  } finally {
    connecting = arriveAtOnce;
    process.off("unhandledRejection", record);
  }
}

test("an abort while connecting rejects with the abort reason even when the native close never settles", async () => {
  const abort = new AbortController();
  const reason = new Error("caller gave up");
  await stopWhileConnectingThenCloseHangs(abort.signal, undefined, async () => abort.abort(reason), error => error === reason);
});

test("a deadline while connecting rejects as exceeded even when the native close never settles", async () => {
  const abort = new AbortController();
  await stopWhileConnectingThenCloseHangs(
    abort.signal,
    1,
    () => new Promise(resolve => setTimeout(resolve, 20)),
    { message: "session deadline exceeded" },
  );
});
