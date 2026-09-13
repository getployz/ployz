"use strict";
// Run against a real daemon: node node_tailcat.js <capability-file> [--uninitialized]
// Rung 2 locally; running the exact artifact on Cloud records runtime evidence.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const { setTimeout: delay } = require("node:timers/promises");
const sdk = require("..");
const capability = fs.readFileSync(process.argv[2], "utf8").trim();
const connections = [{ tailcat: capability }];
const ownHelpers = () => fs.readdirSync("/proc").filter((pid) => {
  if (!/^\d+$/.test(pid)) return false;
  try {
    const status = fs.readFileSync(`/proc/${pid}/status`, "utf8");
    if (!new RegExp(`^PPid:\\s+${process.pid}$`, "m").test(status)) return false;
    const args = fs.readFileSync(`/proc/${pid}/cmdline`, "utf8");
    assert.ok(!args.includes(capability), "capability escaped into child arguments");
    return args.includes("ployz-tailcat");
  } catch (error) { if (error.code === "ENOENT" || error.code === "ESRCH") return false; throw error; }
});
async function reaped(expected) {
  const until = Date.now() + 5000;
  while (ownHelpers().length !== expected && Date.now() < until) await delay(20);
  assert.equal(ownHelpers().length, expected, "helper lifecycle mismatch");
}
let stage = "validation";
async function main() {
  await reaped(0);
  await assert.rejects(sdk.connect({ connections: [{ tailcat: { capability } }] }),
    (error) => error instanceof sdk.RpcError && error.code === "invalid_argument" && !error.message.includes(capability));
  await assert.rejects(sdk.connect({ connections: [] }), (error) => error.code === "invalid_argument");
  const receipt = { node: process.version, platform: process.platform, arch: process.arch, externalDerpOverride: process.env.TS_DEBUG_ALWAYS_USE_DERP !== undefined };
  const started = performance.now();
  const controller = new AbortController();
  stage = "cold connect";
  const first = await sdk.connect({ connections, signal: controller.signal, timeoutMs: 60000 });
  try {
    receipt.coldMs = Math.round(performance.now() - started);
    const warm = performance.now();
    stage = "warm read";
    const about = await first.about();
    receipt.warmReadMs = Math.round(performance.now() - warm);
    receipt.machineId = about.machine_id;
    receipt.daemonVersion = about.daemon_version;
    stage = "independent connect";
    const second = await sdk.connect({ connections, timeoutMs: 60000 });
    try {
      await reaped(2);
      stage = "stream";
      let retainedWatch;
      if (!process.argv.includes("--uninitialized")) {
        const stopWatch = new AbortController();
        const watch = first.runtime.watch({ signal: stopWatch.signal })[Symbol.asyncIterator]();
        const frame = await watch.next();
        assert.equal(frame.done, false);
        assert.ok(frame.value.observer_machine_id || frame.value.machines);
        stopWatch.abort();
        assert.equal((await watch.next()).done, true);
        receipt.streamFrameAndCancellation = true;
        retainedWatch = first.runtime.watch()[Symbol.asyncIterator]();
        assert.equal((await retainedWatch.next()).done, false);
      } else {
        receipt.streamFrameAndCancellation = "not exercised: daemon uninitialized";
      }
      stage = "cancel session";
      controller.abort();
      await reaped(1);
      if (retainedWatch) assert.equal((await retainedWatch.next()).done, true);
      assert.equal((await second.about()).machine_id, about.machine_id);
      receipt.independentSessionSurvivesCancellation = true;
      stage = "connection deadline";
      await assert.rejects(sdk.connect({ connections, timeoutMs: 1 }));
      await reaped(1);
      stage = "session deadline";
      const deadline = await sdk.connect({ connections, timeoutMs: 3000 });
      await delay(3100);
      await assert.rejects(deadline.about());
      await reaped(1);
      receipt.connectionAndSessionDeadlineReaped = true;
      if (!process.argv.includes("--uninitialized")) {
        stage = "retained preview deadline";
        const previewClient = await sdk.connect({ connections, timeoutMs: 3000 });
        const intent = {
          project_name: "tailcat-878-readonly",
          target: [{ name: "web", mode: { mode: "replicated", replicas: 1 }, container: { image: "nginx", pull_policy: "always" } }],
          options: { force_recreate: false, skip_health_monitor: true, placement_seed: 0, selected: [{ name: "web" }] },
        };
        const retainedPreview = await previewClient.preview(intent);
        await reaped(2);
        await delay(3100);
        await reaped(1);
        assert.throws(() => retainedPreview.confirm(), (error) => error.code === "unavailable");
        receipt.retainedPreviewDeadlineReaped = true;

        stage = "blocked read deadline";
        const existing = new Set(ownHelpers());
        const blockedClient = await sdk.connect({ connections, timeoutMs: 2000 });
        const blockedHelper = ownHelpers().find((pid) => !existing.has(pid));
        assert.ok(blockedHelper);
        process.kill(Number(blockedHelper), "SIGSTOP");
        try {
          const blocked = blockedClient.dataLossIfClusterDestroyed();
          await assert.rejects(blocked, (error) => error.code === "unavailable");
          await reaped(1);
          receipt.blockedReadDeadlineReaped = true;
        } finally { await blockedClient.close(); }

        stage = "running deploy deadline";
        const beforeDeploy = new Set(ownHelpers());
        const deployClient = await sdk.connect({ connections, timeoutMs: 3000 });
        try {
          const prepared = await deployClient.preview(intent);
          const helper = ownHelpers().find((pid) => !beforeDeploy.has(pid));
          assert.ok(helper);
          // Stop the transport before confirmation: no mutation reaches the fixture.
          process.kill(Number(helper), "SIGSTOP");
          const running = prepared.confirm();
          await assert.rejects(running.finished, (error) => error instanceof sdk.RpcError && error.code === "unavailable" && error.message.includes("uncertain"));
          await reaped(1);
          receipt.runningDeployDeadlineReaped = true;
        } finally { await deployClient.close(); }

        stage = "read and watch reconnect";
        const prior = new Set(ownHelpers());
        const reconnectClient = await sdk.connect({ connections, timeoutMs: 15000 });
        try {
          const helper = ownHelpers().find((pid) => !prior.has(pid));
          assert.ok(helper);
          process.kill(Number(helper), "SIGKILL");
          await delay(100);
          assert.equal((await reconnectClient.about()).machine_id, about.machine_id);
          const watch = reconnectClient.runtime.watch()[Symbol.asyncIterator]();
          assert.equal((await watch.next()).done, false);
          await reconnectClient.close();
          assert.equal((await watch.next()).done, true);
          await reaped(1);
          assert.equal((await second.about()).machine_id, about.machine_id);
          receipt.sameConnectionReadAndWatchReconnect = true;
        } finally { await reconnectClient.close(); }
      }
    } finally { await second.close(); }
  } finally { await first.close(); }
  await reaped(0);
  receipt.remainingHelpers = 0;
  console.log(JSON.stringify(receipt));
}
main().catch((error) => {
  // Never print transport causes or caller capability data in a receipt.
  console.error(JSON.stringify({ failed: true, stage, name: error.name, message: error.message.replaceAll(capability, "[redacted]") }));
  process.exitCode = 1;
});
