"use strict";
// What the addon says to JavaScript about a malformed Connection list or expected Cluster:
// the error text, which argument fails first, and whether it throws or rejects.
const { test } = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");

const pkg = path.join(__dirname, "..");
const binding = require(path.join(pkg, "ployz-sdk.node"));
const { RpcError, rpc } = require(path.join(pkg, "wrap.js"));

const key = `${"A".repeat(43)}=`;
const tcp = [{ tcp: "127.0.0.1:1" }];
const invalid = message => error =>
  error instanceof RpcError && error.code === "invalid_argument" && error.message === message;

// The default addon refuses a Unix connection as malformed; the `eval` build accepts it on purpose.
const unixAccepted = (() => {
  try {
    rpc(binding.startConnections)([{ unix: "/run/ployz.sock" }]).cancel();
    return true;
  } catch {
    return false;
  }
})();

const MALFORMED = "invalid management connections";
const EMPTY = "connections must not be empty";
const EXPECT = "expect must name a base64 Sealing public key";

test("startConnections throws the refusal for the first argument that fails", () => {
  const start = rpc(binding.startConnections);
  assert.throws(() => start([]), invalid(EMPTY));
  assert.throws(() => start("not a list"), invalid(MALFORMED));
  assert.throws(() => start([{ tcp: 7 }]), invalid(MALFORMED));
  if (!unixAccepted) assert.throws(() => start([{ unix: "/run/ployz.sock" }]), invalid(MALFORMED));
  assert.throws(() => start(tcp, null, null, { sealingPublicKey: "not-a-key" }), invalid(EXPECT));
  assert.throws(() => start(tcp, null, null, "key"), invalid(EXPECT));
  assert.throws(() => start("not a list", null, null, { sealingPublicKey: "not-a-key" }), invalid(MALFORMED));
  assert.throws(() => start([], null, null, { sealingPublicKey: "not-a-key" }), invalid(EXPECT));
});

test("startConnections takes an absent or null expectation as none", () => {
  const start = rpc(binding.startConnections);
  for (const expect of [undefined, null]) {
    const pending = start(tcp, null, null, expect);
    pending.cancel();
  }
  const pending = start(tcp, null, null, { sealingPublicKey: key, allowUnverified: true });
  pending.cancel();
});

for (const [name, call] of [
  ["observeVolumes", (connections, expect) => binding.observeVolumes(connections, [], expect)],
  ["observeCopies", (connections, expect) => binding.observeCopies(connections, expect)],
  ["observeRunning", (connections, expect) => binding.observeRunning(connections, expect)],
]) {
  test(`${name} rejects with the refusal for the first argument that fails`, async () => {
    const run = rpc(call);
    await assert.rejects(run("not a list", null), invalid(MALFORMED));
    if (!unixAccepted) await assert.rejects(run([{ unix: "/run/ployz.sock" }], null), invalid(MALFORMED));
    await assert.rejects(run(tcp, { sealingPublicKey: "not-a-key" }), invalid(EXPECT));
    await assert.rejects(run("not a list", { sealingPublicKey: "not-a-key" }), invalid(MALFORMED));
  });
}

test("observeVolumes refuses a Docker Volume name after its Connections and expectation", async () => {
  const run = rpc((connections, sought, expect) => binding.observeVolumes(connections, sought, expect));
  await assert.rejects(run(tcp, [""], null), invalid("invalid Docker Volume name"));
  await assert.rejects(run(tcp, [""], { sealingPublicKey: "not-a-key" }), invalid(EXPECT));
});
