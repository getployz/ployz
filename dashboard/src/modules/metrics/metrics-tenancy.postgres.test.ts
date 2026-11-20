import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { Effect } from "effect";
import { afterAll, beforeAll, expect, it, vi } from "vitest";
import type { AppConfig } from "#/server/config.server";
import { testConfigLayer } from "#/test/config-environment";
import { DISK_METRIC, HOST_METRICS, SERVICE_METRICS, VOLUME_METRICS, hostHistory, serviceHistory } from "./metrics-history";
import { countCrashEvents, latestReading, readCrashEvents, readDiskRows, readHostRows, readServiceRows, readVolumeRows } from "./metrics-store.server";

const execFile = promisify(execFileCallback);
const docker = (...args: ReadonlyArray<string>) => execFile("docker", args, { maxBuffer: 10 * 1024 * 1024 });

const PASSWORD = "metrics-tenancy-test";
const container = `ployz-metrics-tenancy-${process.pid}-${randomUUID().slice(0, 8)}`;
let clickhouse = "";

const authorization = `Basic ${Buffer.from(`otel:${PASSWORD}`).toString("base64")}`;

async function send(body: string, query = "") {
  const response = await fetch(`${clickhouse}/${query}`, { method: "POST", headers: { authorization }, body });
  const text = await response.text();
  if (!response.ok) throw new Error(text);
}

/** ClickHouse reads a DateTime64 as text in its server's zone, which the container keeps at UTC. */
const clickhouseTime = (seconds: number) => new Date(seconds * 1000).toISOString().replace("T", " ").slice(0, 19);

const insert = (table: string, rows: ReadonlyArray<object>) =>
  send(rows.map((row) => JSON.stringify(row)).join("\n"), `?query=${encodeURIComponent(`INSERT INTO otel.${table} FORMAT JSONEachRow`)}`);

beforeAll(async () => {
  await docker("run", "--detach", "--rm", "--name", container, "--publish", "127.0.0.1::8123", "--tmpfs", "/var/lib/clickhouse:rw",
    "--env", "CLICKHOUSE_USER=otel", "--env", `CLICKHOUSE_PASSWORD=${PASSWORD}`, "--env", "CLICKHOUSE_DB=otel", "clickhouse/clickhouse-server:25.8");
  const { stdout } = await docker("port", container, "8123/tcp");
  clickhouse = `http://127.0.0.1:${stdout.trim().split("\n")[0]?.split(":").at(-1)}`;
  await vi.waitFor(async () => {
    if (!(await fetch(`${clickhouse}/ping`)).ok) throw new Error("not yet");
  }, { timeout: 60_000, interval: 1000 });
  await execFile(process.execPath, ["scripts/observe-migrate.ts"], {
    cwd: process.cwd(),
    env: { ...process.env, OBSERVE_CLICKHOUSE_URL: clickhouse, CLICKHOUSE_USER: "otel", CLICKHOUSE_PASSWORD: PASSWORD },
  });
  await seed();
}, 180_000);

afterAll(async () => {
  await docker("rm", "--force", container).catch(() => undefined);
}, 60_000);

const NAMESPACE = "shop-production";
const SERVICE = "web";
/** Both Organizations run the same Machine id, as two clusters can: only the Organization tells their rows apart. */
const MACHINE = "machine-1";
const ORGANIZATIONS = ["org-a", "org-b"] as const;
const NOW = Math.floor(Date.now() / 1000);
const FAN_MACHINE = "machine-fan";
const FAN_SERVICE = "fan";
const LOOP_SERVICE = "loop";
const LOOP_EVENTS = 590;
/** Two buckets of ten seconds that the read window below covers. */
const TIMES = [NOW - 120, NOW - 60] as const;

const VOLUME = `${NAMESPACE}_vol-data`;
const EDGE_SERVICE = "edge";
/** A minute bucket that starts before the read window's second bucket, for samples placed either side of its edges. */
const EDGE = Math.floor((NOW - 240) / 60) * 60;

const ROLL_SERVICE = "roll";
const ROLL_MACHINE = "machine-roll";
/** A minute bucket inside the read window, in which one replica gives way to the next with no overlap. */
const ROLL = Math.floor((NOW - 200) / 60) * 60;
const ROLL_MEMORY = 256;
const ROLL_MACHINE_MEMORY = 4100;

/** Each Organization's CPU counter grows by its own amount, so a leaked row changes the sum. */
const GROWTH = { "org-a": 10, "org-b": 500 } as const;

async function seed() {
  const resource = (org: string, extra: Readonly<Record<string, string>> = {}) => ({
    "ployz.org": org, "ployz.machine": MACHINE, "ployz.namespace": NAMESPACE, "ployz.service.name": SERVICE, "container.id": `${org}-container`, ...extra,
  });
  await insert("otel_metrics_sum", ORGANIZATIONS.flatMap((org) => TIMES.flatMap((time, index) => [
    { ResourceAttributes: resource(org), MetricName: SERVICE_METRICS.cpu, Attributes: {}, TimeUnix: clickhouseTime(time), Value: 100 + index * GROWTH[org], IsMonotonic: true },
    { ResourceAttributes: resource(org, { "container.id": "" }), MetricName: HOST_METRICS.memoryTotal, Attributes: {}, TimeUnix: clickhouseTime(time), Value: GROWTH[org] * 1e6, IsMonotonic: false },
  ])));
  await insert("otel_metrics_gauge", ORGANIZATIONS.map((org) => ({
    ResourceAttributes: { "ployz.org": org, "ployz.machine": MACHINE }, MetricName: DISK_METRIC, Attributes: { kind: "volumes" }, TimeUnix: clickhouseTime(TIMES[1]), Value: GROWTH[org],
  })));
  // Counters that fan out: a Machine of two cores, one pinned and one idle, and a Service reading two block devices.
  const counter = (index: number, grow: number) => 1000 + index * grow;
  await insert("otel_metrics_sum", TIMES.flatMap((time, index) => [
    ...[["cpu0", "user", 60], ["cpu0", "idle", 0], ["cpu1", "user", 0], ["cpu1", "idle", 60]].map(([cpu, state, grow]) => ({
      ResourceAttributes: { "ployz.org": "org-a", "ployz.machine": FAN_MACHINE }, MetricName: HOST_METRICS.cpu,
      Attributes: { cpu, state }, TimeUnix: clickhouseTime(time), Value: counter(index, Number(grow)), IsMonotonic: true,
    })),
    ...["8", "253"].map((major) => ({
      ResourceAttributes: { "ployz.org": "org-a", "ployz.machine": FAN_MACHINE, "ployz.namespace": NAMESPACE, "ployz.service.name": FAN_SERVICE, "container.id": "fan-container" },
      MetricName: SERVICE_METRICS.io, Attributes: { device_major: major, device_minor: "0", operation: "read" },
      TimeUnix: clickhouseTime(time), Value: counter(index, 60e6), IsMonotonic: true,
    })),
  ]));
  // Each Organization's managed Volume writes its own amount, read by ployzd with no container, and another Volume of
  // the Namespace writes too.
  await insert("otel_metrics_sum", ORGANIZATIONS.flatMap((org) => TIMES.flatMap((time, index) => ["vol-data", "vol-other"].map((volume) => ({
    ResourceAttributes: { "ployz.org": org, "ployz.machine": MACHINE, "service.name": "ployzd" }, MetricName: VOLUME_METRICS.write,
    Attributes: { "ployz.namespace": NAMESPACE, "ployz.volume": volume }, TimeUnix: clickhouseTime(time), Value: index * GROWTH[org] * 60e6, IsMonotonic: true,
  })))));
  // 100 bytes a second, the last scrape of one bucket a second before its edge, of the next a second after it.
  await insert("otel_metrics_sum", [-1, 1, 61, 119].map((offset) => ({
    ResourceAttributes: { "ployz.org": "org-a", "ployz.machine": MACHINE, "ployz.namespace": NAMESPACE, "ployz.service.name": EDGE_SERVICE, "container.id": "edge-container" },
    MetricName: SERVICE_METRICS.rx, Attributes: { interface: "eth0" }, TimeUnix: clickhouseTime(EDGE + offset), Value: (offset + 1) * 100, IsMonotonic: true,
  })));
  // A rollout: the old replica reports 256 for half of one bucket and its replacement 256 for the other half. Neither
  // has a limit of its own, so Docker reports the Machine's memory for both. Another Organization runs a Machine of the
  // same id with a smaller total, which must not mix in.
  const replica = (container: string, offsets: readonly number[]) => offsets.flatMap((offset) => [SERVICE_METRICS.memory, SERVICE_METRICS.memoryLimit].map((metric) => ({
    ResourceAttributes: { "ployz.org": "org-a", "ployz.machine": ROLL_MACHINE, "ployz.namespace": NAMESPACE, "ployz.service.name": ROLL_SERVICE, "container.id": container },
    MetricName: metric, Attributes: {}, TimeUnix: clickhouseTime(ROLL + offset), Value: metric === SERVICE_METRICS.memory ? ROLL_MEMORY : ROLL_MACHINE_MEMORY, IsMonotonic: false,
  })));
  await insert("otel_metrics_sum", [
    ...replica("roll-old", [0, 10, 20]),
    ...replica("roll-new", [30, 40, 50]),
    ...[["org-a", ROLL_MACHINE_MEMORY], ["org-b", 99]].map(([org, total]) => ({
      ResourceAttributes: { "ployz.org": org, "ployz.machine": ROLL_MACHINE }, MetricName: HOST_METRICS.memoryTotal, Attributes: {}, TimeUnix: clickhouseTime(ROLL), Value: total, IsMonotonic: false,
    })),
  ]);
  // One replica exiting 1 every 6 s for the last hour, newest 10 s ago.
  await insert("otel_logs", Array.from({ length: LOOP_EVENTS }, (_, index) => ({
    Timestamp: clickhouseTime(NOW - 10 - index * 6), Body: "container exited", ResourceAttributes: { "ployz.org": "org-a", "ployz.machine": FAN_MACHINE },
    LogAttributes: {
      "ployz.namespace": NAMESPACE, "ployz.service.name": LOOP_SERVICE, "ployz.service.id": "loop-service", "container.id": "loop-container",
      "ployz.exit.reason": "exit_nonzero", "ployz.exit.code": "1", "ployz.event.id": `loop-${index}`,
    },
  })));
  await insert("otel_logs", ORGANIZATIONS.map((org) => ({
    Timestamp: clickhouseTime(TIMES[1]), Body: "container exited", ResourceAttributes: { "ployz.org": org, "ployz.machine": MACHINE },
    LogAttributes: {
      "ployz.namespace": NAMESPACE, "ployz.service.name": SERVICE, "ployz.service.id": `${org}-service`, "container.id": `${org}-container`,
      "ployz.exit.reason": "out_of_memory", "ployz.event.id": `${org}-event`,
    },
  })));
}

const range = { window: { from: (NOW - 300) * 1000, to: NOW * 1000 }, stepMs: 60_000 };

/** Runs a read as the session's Organization against the seeded ClickHouse. The reads never touch Postgres, so its URL is a placeholder. */
const asOrganization = <A, E>(read: Effect.Effect<A, E, AppConfig>) =>
  Effect.runPromise(read.pipe(Effect.provide(testConfigLayer({ DATABASE_URL: "postgres://unused:unused@127.0.0.1:9/unused", OBSERVE_CLICKHOUSE_URL: clickhouse, CLICKHOUSE_USER: "otel", CLICKHOUSE_PASSWORD: PASSWORD }))));

it("reads a Service's rows for the session's Organization alone when two Organizations share a namespace and Service name", async () => {
  for (const org of ORGANIZATIONS) {
    const rows = await asOrganization(readServiceRows(org, range, NAMESPACE, SERVICE));
    expect(rows.map((row) => row.series)).toEqual([`${org}-container|||`, `${org}-container|||`]);
    expect(rows.map((row) => row.value)).toEqual([100, 100 + GROWTH[org]]);
  }
  expect(await asOrganization(readServiceRows("org-c", range, NAMESPACE, SERVICE))).toEqual([]);
}, 60_000);

it("reads a Server's rows and disk for the session's Organization alone when two Organizations run a Machine of the same id", async () => {
  for (const org of ORGANIZATIONS) {
    const rows = await asOrganization(readHostRows(org, range, MACHINE));
    expect(rows.map((row) => row.value)).toEqual([GROWTH[org] * 1e6, GROWTH[org] * 1e6]);
    expect((await asOrganization(readDiskRows(org, range, MACHINE))).map((row) => row.value)).toEqual([GROWTH[org]]);
  }
  expect(await asOrganization(readHostRows("org-c", range, MACHINE))).toEqual([]);
}, 60_000);

it("reads crash events for the session's Organization alone", async () => {
  for (const org of ORGANIZATIONS) {
    const crashes = await asOrganization(readCrashEvents(org, range, { namespace: NAMESPACE, service: SERVICE }));
    expect(crashes.map((crash) => [crash.container, crash.serviceId, crash.reason])).toEqual([[`${org}-container`, `${org}-service`, "out_of_memory"]]);
  }
  expect(await asOrganization(readCrashEvents("org-c", range, { namespace: NAMESPACE, service: SERVICE }))).toEqual([]);
}, 60_000);

it("reads each core's state, each block device and each interface as its own series, so growth sums across them", async () => {
  const reading = { ...range, at: NOW * 1000 };
  const host = await asOrganization(readHostRows("org-a", range, FAN_MACHINE));
  expect(new Set(host.map((row) => row.series)).size).toBe(4);
  expect(hostHistory(host, [], reading).points.map((point) => point.cpuBusy)).toEqual([0.5]);
  const service = await asOrganization(readServiceRows("org-a", range, NAMESPACE, FAN_SERVICE));
  expect(new Set(service.map((row) => row.series)).size).toBe(2);
  expect(serviceHistory(service, [], reading).points.map((point) => point.diskRead)).toEqual([2e6]);
}, 60_000);

it("keeps the newest crash events and counts every one when a replica crashes more often than the read lists", async () => {
  const hour = { window: { from: (NOW - 3600) * 1000, to: NOW * 1000 }, stepMs: 20_000 };
  const filter = { namespace: NAMESPACE, service: LOOP_SERVICE };
  const crashes = await asOrganization(readCrashEvents("org-a", hour, filter));
  expect(crashes).toHaveLength(500);
  expect(crashes.at(-1)?.at).toBe((NOW - 10) * 1000);
  expect(crashes.map((crash) => crash.at)).toEqual([...crashes.map((crash) => crash.at)].sort((left, right) => left - right));
  expect(await asOrganization(countCrashEvents("org-a", hour, filter))).toEqual({ oomKills: 0, exits: LOOP_EVENTS });
  expect(await asOrganization(countCrashEvents("org-a", hour, { ...filter, namespaces: [NAMESPACE] }))).toEqual({ oomKills: 0, exits: LOOP_EVENTS });
  expect(await asOrganization(countCrashEvents("org-a", hour, { ...filter, namespaces: ["elsewhere"] }))).toEqual({ oomKills: 0, exits: 0 });
  expect(await asOrganization(countCrashEvents("org-a", hour, { ...filter, namespaces: [] }))).toEqual({ oomKills: 0, exits: 0 });
  expect(await asOrganization(countCrashEvents("org-a", hour, { machine: "another-machine" }))).toEqual({ oomKills: 0, exits: 0 });
  expect(await asOrganization(countCrashEvents("org-b", hour, filter))).toEqual({ oomKills: 0, exits: 0 });
}, 60_000);

it("reads a managed Volume's writes for the session's Organization alone, once, into the Service's disk", async () => {
  const reading = { ...range, at: NOW * 1000 };
  for (const org of ORGANIZATIONS) {
    const volumes = await asOrganization(readVolumeRows(org, range, [VOLUME]));
    expect(volumes.map((row) => [row.series, row.value])).toEqual([[VOLUME, 0], [VOLUME, GROWTH[org] * 60e6]]);
    const rows = [...await asOrganization(readServiceRows(org, range, NAMESPACE, SERVICE)), ...volumes];
    expect(serviceHistory(rows, [], reading).points.at(-1)?.diskWrite).toBe(GROWTH[org] * 1e6);
  }
  expect(await asOrganization(readVolumeRows("org-c", range, [VOLUME]))).toEqual([]);
  expect(await asOrganization(readVolumeRows("org-a", range, []))).toEqual([]);
}, 60_000);

it("divides a counter's growth by the time between its samples, so a scrape either side of a bucket edge reads the steady rate", async () => {
  const rows = await asOrganization(readServiceRows("org-a", range, NAMESPACE, EDGE_SERVICE));
  const rates = serviceHistory(rows, [], { ...range, at: NOW * 1000 }).points.map((point) => point.netIn);
  expect(rates).toHaveLength(2);
  for (const rate of rates) expect(rate).toBeCloseTo(100, 6);
}, 60_000);

it("reads a rollout with no overlap as one replica's memory, and a limit that is the Machine's memory as none", async () => {
  const rows = await asOrganization(readServiceRows("org-a", range, NAMESPACE, ROLL_SERVICE));
  const [point] = serviceHistory(rows, [], { ...range, at: NOW * 1000 }).points.filter((entry) => entry.at === ROLL * 1000);
  expect(point?.memory).toBe(ROLL_MEMORY);
  expect(point?.memoryLimit).toBeUndefined();
  expect(await asOrganization(readServiceRows("org-b", range, NAMESPACE, ROLL_SERVICE))).toEqual([]);
}, 60_000);

it("reads when a Machine or Service last sent a reading, for the session's Organization alone", async () => {
  const since = (NOW - 300) * 1000;
  expect(await asOrganization(latestReading("org-a", { machine: FAN_MACHINE }, since))).toBe(TIMES[1] * 1000);
  expect(await asOrganization(latestReading("org-b", { namespace: NAMESPACE, service: SERVICE }, since))).toBe(TIMES[1] * 1000);
  expect(await asOrganization(latestReading("org-c", { machine: MACHINE }, since))).toBeNull();
  expect(await asOrganization(latestReading("org-a", { machine: MACHINE }, (NOW - 30) * 1000))).toBeNull();
}, 60_000);
