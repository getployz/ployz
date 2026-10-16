// Simulates hosts waiting on a Fleet Token: each gets its own ed25519 Check-in Key and posts one signed check-in per
// tick to /api/checkin, then the run prints answers, p50, p95 and errors. Run from dashboard/ against up.sh's Cloud:
//   node scripts/verify/checkin-sim.mjs --token pflt_... [--hosts N] [--ticks N | --once] [--interval ms]
//     [--bad-signature] [--skew seconds] [--prep docker=installing,zfs=installing] [--hostname name]
//     [--pad-bytes N] [--ip addr] [--url http://localhost:port]
// Each host claims its own source address through x-forwarded-for (198.18.0.0/15), which dev Cloud trusts; --ip
// sends every host from one address instead.
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";

const { values: flags } = parseArgs({
  options: {
    token: { type: "string" },
    hosts: { type: "string", default: "1" },
    ticks: { type: "string", default: "1" },
    once: { type: "boolean", default: false },
    interval: { type: "string", default: "1000" },
    "bad-signature": { type: "boolean", default: false },
    skew: { type: "string", default: "0" },
    prep: { type: "string" },
    hostname: { type: "string" },
    "pad-bytes": { type: "string", default: "0" },
    ip: { type: "string" },
    url: { type: "string" },
  },
});
if (flags.token === undefined) throw new Error("checkin-sim: --token pflt_... is required");

const baseUrl = flags.url ?? (() => {
  const info = readFileSync(new URL("../../.verify/run/info", import.meta.url), "utf8");
  const match = /^Ployz Cloud verify: (\S+)/m.exec(info);
  if (match === null) throw new Error("checkin-sim: no Cloud in .verify/run/info; run scripts/verify/up.sh or pass --url");
  return match[1];
})();
const hostCount = Number(flags.hosts);
const ticks = flags.once ? 1 : Number(flags.ticks);
const interval = Number(flags.interval);
const prep = flags.prep === undefined ? undefined : Object.fromEntries(flags.prep.split(",").map((pair) => pair.split("=")));

const hosts = Array.from({ length: hostCount }, (_, index) => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  return {
    key: Buffer.from(publicKey.export({ format: "jwk" }).x, "base64url").toString("hex"),
    privateKey,
    hostname: flags.hostname ?? `sim-${index + 1}`,
    ip: flags.ip ?? `198.18.${Math.floor(index / 250)}.${(index % 250) + 1}`,
  };
});

function report(host) {
  return {
    hostname: host.hostname,
    os: { id: "debian", version: "12" },
    kernel: "6.1.0",
    arch: "x86_64",
    virt: "kvm",
    cpuCount: 4,
    memoryTotalBytes: 8_589_934_592,
    disks: [{ path: "/dev/vda", sizeBytes: 85_899_345_920, role: "root" }],
    docker: { kind: "absent" },
    cgroup: "v2",
    zfs: { kind: "ineligible", code: "no_disk", message: "No empty disk." },
    blockers: [],
    warnings: Number(flags["pad-bytes"]) > 0 ? [{ code: "padding", message: "x".repeat(Number(flags["pad-bytes"])) }] : [],
  };
}

function signed(host) {
  const body = Buffer.from(JSON.stringify({
    protocolVersion: 1,
    checkinKey: host.key,
    cliVersion: "checkin-sim",
    report: report(host),
    ...(prep === undefined ? {} : { prep }),
  }));
  const timestamp = BigInt(Math.floor(Date.now() / 1000) + Number(flags.skew));
  const stamp = Buffer.alloc(8);
  stamp.writeBigInt64BE(timestamp);
  const message = Buffer.concat([
    Buffer.from("ployz-checkin-v1\0", "ascii"),
    Buffer.from(host.key, "ascii"),
    Buffer.from([0]),
    stamp,
    createHash("sha256").update(body).digest(),
  ]);
  const signature = sign(null, message, host.privateKey);
  if (flags["bad-signature"]) signature[0] ^= 0xff;
  return {
    body,
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${flags.token}`,
      "ployz-timestamp": String(timestamp),
      "ployz-signature": `v1=${signature.toString("base64url")}`,
      "x-forwarded-for": host.ip,
    },
  };
}

function describe(status, answer) {
  if (status === 200 && answer.kind === "wait") return `wait retryAfter=${answer.retryAfter}`;
  if (status === 200 && answer.kind === "stop") return `stop ${answer.reason}`;
  return `${status} ${answer.error ?? answer.message ?? JSON.stringify(answer)}`;
}

const latencies = [];
const answers = new Map();
let errors = 0;

async function checkIn(host) {
  const { body, headers } = signed(host);
  const started = performance.now();
  try {
    const response = await fetch(`${baseUrl}/api/checkin`, { method: "POST", body, headers });
    const text = await response.text();
    latencies.push(performance.now() - started);
    const answer = describe(response.status, JSON.parse(text));
    if (response.status !== 200) errors += 1;
    answers.set(answer, (answers.get(answer) ?? 0) + 1);
  } catch (error) {
    errors += 1;
    answers.set(`failed ${error.message}`, (answers.get(`failed ${error.message}`) ?? 0) + 1);
  }
}

for (let tick = 0; tick < ticks; tick += 1) {
  const started = Date.now();
  const spread = hostCount > 1 && !flags.once ? interval : 0;
  await Promise.all(hosts.map((host) =>
    new Promise((resolve) => setTimeout(resolve, Math.random() * spread)).then(() => checkIn(host))));
  if (tick < ticks - 1) await new Promise((resolve) => setTimeout(resolve, Math.max(0, interval - (Date.now() - started))));
}

const percentile = (p) => {
  const sorted = [...latencies].sort((a, b) => a - b);
  return sorted.length === 0 ? 0 : sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)];
};
for (const [answer, count] of answers) console.log(hostCount * ticks === 1 ? answer : `${count}x ${answer}`);
if (hostCount * ticks > 1) {
  console.log(`requests=${hostCount * ticks} p50=${percentile(50).toFixed(1)}ms p95=${percentile(95).toFixed(1)}ms errors=${errors}`);
}
process.exitCode = errors > 0 ? 1 : 0;
