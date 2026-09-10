"use strict";
// Rung 4: disposable installed, participating systemd Machine only.
// PLOYZ_SDK_PACKAGE=/built/sdk node node_tailcat_removal.js < protected-input.json
// Input: {expected, expected_pairing, machine_id, ssh, ssh_key_file?, ssh_known_hosts?}.
// Creates/removes a Docker sentinel and changes this Machine's Cloud Pairing.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { spawn } = require("node:child_process");
const { randomBytes, createHash } = require("node:crypto");
const { Duplex } = require("node:stream");
const http2 = require("node:http2");
const { setTimeout: delay } = require("node:timers/promises");
const packageDir = path.resolve(process.env.PLOYZ_SDK_PACKAGE || path.join(__dirname, ".."));
const sdk = require(packageDir);
let stage = "input";
const receipt = { rung: 4, sdkSha256: createHash("sha256").update(fs.readFileSync(path.join(packageDir, "ployz-sdk.node"))).digest("hex"),
  clientHelperSha256: createHash("sha256").update(fs.readFileSync(path.join(packageDir, "ployz-tailcat"))).digest("hex") };
const sessions = [];
const children = [];
let config;
let sentinel;
let fixturePairing = false;
function bounded(promise, milliseconds = 45000) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error("stage deadline exceeded")), milliseconds);
  })]).finally(() => clearTimeout(timer));
}
function ssh(args) {
  return ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=10",
    ...(config.ssh_key_file ? ["-i", config.ssh_key_file] : []),
    ...(config.ssh_known_hosts ? ["-o", `UserKnownHostsFile=${config.ssh_known_hosts}`] : []),
    config.ssh, ...args];
}
function start(program, args) {
  const child = spawn(program, args, { stdio: ["pipe", "pipe", "ignore"] });
  children.push(child);
  child.on("error", () => {});
  return child;
}
async function shell(script) {
  const child = start("ssh", ssh(["bash", "-s", "--", sentinel]));
  const output = [];
  child.stdout.on("data", data => output.push(data));
  child.stdin.on("error", () => {});
  child.stdin.end(script);
  const code = await bounded(new Promise((resolve, reject) => {
    child.once("exit", resolve); child.once("error", reject);
  }));
  assert.equal(code, 0, "SSH fixture command failed");
  return Buffer.concat(output).toString();
}
// Existing Machine RPC framing: gRPC -> protobuf bytes field 1 -> JSON envelope.
function frame(command, payload) {
  const json = Buffer.from(JSON.stringify({ protocol_major: 1, command, payload }));
  let length = json.length;
  const varint = [];
  do { varint.push((length & 127) | (length > 127 ? 128 : 0)); length >>>= 7; } while (length);
  const protobuf = Buffer.concat([Buffer.from([10, ...varint]), json]);
  const header = Buffer.alloc(5); header.writeUInt32BE(protobuf.length, 1);
  return Buffer.concat([header, protobuf]);
}
function rpcStream(child, route, command, payload) {
  const transport = Duplex.from({ readable: child.stdout, writable: child.stdin });
  const session = http2.connect("http://localhost", { createConnection: () => transport });
  session.on("error", () => {});
  sessions.push(session);
  const request = session.request({ ":method": "POST", ":path": `/ployz.rpc.v1.MachineRpc/${route}`,
    "content-type": "application/grpc", te: "trailers" });
  request.on("error", () => {});
  request.end(frame(command, payload));
  return { request, session };
}
async function setPairing(secret) {
  const child = start("ssh", ssh(["ployzd", "dial-stdio"]));
  const { request, session } = rpcStream(child, "SetCloudPairing", "set_cloud_pairing",
    secret === null ? { kind: "clear" } : { kind: "set", pairing: { secret } });
  try {
    const chunks = [];
    request.on("data", data => chunks.push(data));
    await bounded(new Promise((resolve, reject) => { request.once("end", resolve); request.once("error", reject); }));
    const bytes = Buffer.concat(chunks);
    assert.equal(bytes[0], 0); assert.equal(bytes[5], 10);
    let offset = 6;
    while (bytes[offset++] & 128) { assert.ok(offset < bytes.length); }
    const result = JSON.parse(bytes.subarray(offset).toString());
    assert.equal(result.kind, "cloud_pairing_set", "fixture pairing RPC failed");
  } finally { session.destroy(); child.kill(); }
}
const snapshotScript = `set -euo pipefail
python3 - "$1" <<'PY'
import json, os, pwd, grp, stat, subprocess, sys
run = lambda *args: subprocess.check_output(args, text=True).strip()
state = os.stat('/var/lib/ployz/tailcat/state.json')
assert state.st_uid == pwd.getpwnam('ployz').pw_uid
assert state.st_gid == grp.getgrnam('ployz').gr_gid
assert stat.S_IMODE(state.st_mode) == 0o600
run('systemctl', 'is-active', '--quiet', 'ployz.service', 'ployz-tailcat.service')
workload = json.loads(run('docker', 'inspect', sys.argv[1]))[0]
assert workload['State']['Running']
links = json.loads(run('ip', '-j', 'link', 'show', 'type', 'wireguard'))
assert links, 'participating fixture must have kernel WireGuard'
print(json.dumps(dict(daemon_pid=run('systemctl','show','--property=MainPID','--value','ployz.service'),
helper_pid=run('systemctl','show','--property=MainPID','--value','ployz-tailcat.service'),
workload_id=workload['Id'], workload_started=workload['State']['StartedAt'],
workload_image=workload['Image'], workload_digests=json.loads(run('docker','image','inspect',workload['Image']))[0]['RepoDigests'],
wireguard=[{k: x[k] for k in ('ifindex','ifname','address') if k in x} for x in links],
daemon_sha256=run('sha256sum','/usr/local/bin/ployzd').split()[0], helper_sha256=run('sha256sum','/usr/local/bin/ployzd-tailcat').split()[0],
state_uid=state.st_uid,state_gid=state.st_gid,state_mode=stat.S_IMODE(state.st_mode))))
PY
`;
async function connect(capability, machineId = config.machine_id) {
  const client = await sdk.connect({ connections: [{ tailcat: capability, machine_id: machineId }], timeoutMs: 180000 });
  return client;
}
async function main() {
  config = JSON.parse(fs.readFileSync(0, "utf8"));
  for (const key of ["expected", "expected_pairing", "machine_id", "ssh"]) assert.equal(typeof config[key], "string");
  assert.match(config.ssh, /^[a-zA-Z0-9][a-zA-Z0-9_.@:-]*$/);
  sentinel = `tailcat-881-${randomBytes(8).toString("hex")}`;
  stage = "SSH fixture setup";
  await shell('set -euo pipefail\ndocker run -d --name "$1" --label tailcat-qualification alpine:3.22 sleep 900 >/dev/null\n');
  const before = JSON.parse(await shell(snapshotScript));
  fixturePairing = true;
  await setPairing(config.expected_pairing);
  stage = "old identity";
  const old = await connect(config.expected);
  sessions.push(old);
  const initial = await old.inspect();
  assert.equal(initial.id, config.machine_id);
  assert.equal(initial.phase, "participating");
  assert.equal(initial.cloud_paired, true);
  const successor = await sdk.prepareTailcatRemoval(config.expected);
  const removal = { expected: config.expected, successor, expected_pairing: config.expected_pairing };
  stage = "active old stream";
  const active = start(path.join(packageDir, "ployz-tailcat"), ["connect"]);
  const exited = new Promise((resolve, reject) => { active.once("exit", resolve); active.once("error", reject); });
  active.stdin.write(config.expected + "\n");
  const { request } = rpcStream(active, "RuntimeWatch", "runtime_watch", {});
  await bounded(new Promise((resolve, reject) => { request.once("data", resolve); request.once("error", reject); }));
  await delay(500);
  assert.equal(active.exitCode, null, "old stream ended before removal");
  stage = "rotation through old endpoint";
  // Losing this reply is expected; confirmation comes from a different credential.
  await bounded(old.removeCloudPairing(removal).catch(() => {}));
  await bounded(exited);
  receipt.oldActiveStreamClosed = true;
  stage = "successor confirmation";
  let next;
  for (let attempt = 0; attempt < 4 && !next; attempt++) {
    try { next = await connect(successor); } catch { await delay(1000); }
  }
  assert.ok(next, "successor unavailable"); sessions.push(next);
  const inspected = await next.inspect();
  assert.equal(inspected.id, config.machine_id); assert.equal(inspected.cloud_paired, false);
  receipt.successorIdentityAndPairingConfirmed = true;
  stage = "old capability denied";
  await assert.rejects(sdk.connect({ connections: [{ tailcat: config.expected, machine_id: config.machine_id }], timeoutMs: 12000 }));
  stage = "wrong Machine denied";
  await assert.rejects(sdk.connect({ connections: [{ tailcat: successor, machine_id: randomBytes(16).toString("hex") }], timeoutMs: 15000 }));
  receipt.oldCapabilityAndWrongMachineDenied = true;
  stage = "idempotent retry";
  const rotated = JSON.parse(await shell(snapshotScript));
  await bounded(next.removeCloudPairing(removal).catch(() => {}));
  // A live unit alone can be the old process before systemd begins its job.
  let after;
  await bounded((async () => {
    while (!after || after.helper_pid === rotated.helper_pid) {
      try { after = JSON.parse(await shell(snapshotScript)); } catch {}
      if (!after || after.helper_pid === rotated.helper_pid) await delay(500);
    }
  })());
  assert.notEqual(before.helper_pid, rotated.helper_pid);
  assert.notEqual(rotated.helper_pid, after.helper_pid);
  for (const key of ["daemon_pid", "workload_id", "workload_started", "wireguard", "state_uid", "state_gid", "state_mode"]) assert.deepEqual(after[key], before[key]);
  receipt.retryRestartAndSshWorkloadWireguardSurvival = true;
  receipt.before = before;
  receipt.after = after;
  stage = "stale pairing after re-enrollment";
  await setPairing(randomBytes(32).toString("hex"));
  const rejoined = await connect(successor); sessions.push(rejoined);
  await assert.rejects(rejoined.removeCloudPairing(removal));
  assert.equal((await rejoined.inspect()).cloud_paired, true);
  await assert.rejects(sdk.connect({ connections: [{ tailcat: config.expected, machine_id: config.machine_id }], timeoutMs: 12000 }));
  receipt.stalePairingRejected = true;
  receipt.oldCapabilityDeniedAfterRestartAndReenrollment = true;
  receipt.machineId = config.machine_id;
}
(async () => {
  try { await main(); } catch { receipt.failed = true; receipt.stage = stage; process.exitCode = 1; }
  finally {
    for (const session of sessions) {
      try { if (session.destroy) session.destroy(); else await session.close(); } catch {}
    }
    for (const child of children) child.kill();
    if (fixturePairing) { try { await setPairing(null); receipt.fixturePairingCleared = true; } catch { receipt.cleanupPairingFailed = true; process.exitCode = 1; } }
    if (sentinel) { try { await shell('docker rm -f "$1" >/dev/null\n'); receipt.fixtureWorkloadRemoved = true; } catch { receipt.cleanupWorkloadFailed = true; process.exitCode = 1; } }
    console.log(JSON.stringify(receipt));
  }
})();
