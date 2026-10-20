"use strict";
const { test, after } = require("node:test");
const assert = require("node:assert/strict");
const { getEventListeners } = require("node:events");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const Module = require("node:module");

const pkg = path.join(__dirname, "..");
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-connect-"));
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
const native = new Module(addon);
native.loaded = true;
native.exports = {
  Client: NativeClient,
  ConfigStore: class {},
  SealedNames: class {},
  startConnections: () => ({ cancel() {}, wait: async () => (session = new NativeClient()) }),
};
require.cache[addon] = native;
const sdk = require(dir);

const abortListeners = signal => getEventListeners(signal, "abort").length;
const tick = () => new Promise(resolve => setImmediate(resolve));

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
