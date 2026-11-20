import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { Effect } from "effect";
import { afterAll, beforeAll, expect, it, vi } from "vitest";
import type { AppConfig } from "#/server/config.server";
import { testConfigLayer } from "#/test/config-environment";
import { DISK_METRIC, HOST_METRICS, SERVICE_METRICS } from "./metrics-history";
import { readCrashEvents, readDiskRows, readHostRows, readServiceRows } from "./metrics-store.server";

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
/** Two buckets of ten seconds that the read window below covers. */
const TIMES = [NOW - 120, NOW - 60];

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
    expect(rows.map((row) => row.series)).toEqual([`${org}-container`, `${org}-container`]);
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
