import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { Effect, Redacted } from "effect";
import { afterAll, beforeAll, expect, it, vi } from "vitest";
import type { ClickhouseEndpoint } from "#/modules/observe-store/clickhouse.server";
import { type LogHistoryRow, type LogHistoryScope, readLogHistory } from "./log-history.server";

const execFile = promisify(execFileCallback);
const docker = (...args: ReadonlyArray<string>) => execFile("docker", args, { maxBuffer: 10 * 1024 * 1024 });

const PASSWORD = "history-test";
const run = `ployz-log-history-test-${process.pid}-${randomUUID().slice(0, 8)}`;
let endpoint: ClickhouseEndpoint;

const ACME = "00000000-0000-4000-8000-0000000000a1";
const OTHER = "00000000-0000-4000-8000-0000000000b2";
const NAMESPACE = "shop-prod";
const SERVICE_ID = "11111111-1111-4111-8111-111111111111";
// Rows older than their retention are dropped, so the clock is the real one.
const NOW = new Date(Math.floor(Date.now() / 1000) * 1000);
const NOW_NANOS = BigInt(NOW.getTime()) * 1_000_000n;

type Seed = { org: string; at: bigint; body: string; machine?: string; container?: string; stream?: string; service?: string };

const insert = async (rows: ReadonlyArray<Seed>) => {
  const values = rows.map((row) => JSON.stringify({
    Timestamp: `${row.at / 1_000_000_000n}.${String(row.at % 1_000_000_000n).padStart(9, "0")}`,
    Body: row.body,
    ResourceAttributes: { "ployz.org": row.org, "ployz.machine": row.machine ?? "m-one", "ployz.retention_days": "30" },
    LogAttributes: {
      "ployz.namespace": NAMESPACE, "ployz.service.name": row.service ?? "web", "ployz.service.id": SERVICE_ID,
      "container.id": row.container ?? "c-one", "log.iostream": row.stream ?? "stdout",
    },
  })).join("\n");
  await send("INSERT INTO otel.otel_logs (Timestamp, Body, ResourceAttributes, LogAttributes) FORMAT JSONEachRow", values);
};

async function send(sql: string, body?: string) {
  const target = new URL("/", endpoint.url);
  if (body !== undefined) target.searchParams.set("query", sql);
  const response = await fetch(target, {
    method: "POST",
    headers: { authorization: `Basic ${Buffer.from(`otel:${PASSWORD}`).toString("base64")}` },
    body: body ?? sql,
  });
  const text = await response.text();
  if (!response.ok) throw new Error(text);
  return text;
}

const read = (scope: Partial<LogHistoryScope>, options: { limit?: number; cursor?: string } = {}) =>
  Effect.runPromise(readLogHistory(endpoint, { organizationId: ACME, namespace: NAMESPACE, ...scope }, { limit: 500, now: NOW, ...options }));

const pages = async (scope: Partial<LogHistoryScope>, limit: number) => {
  const rows: LogHistoryRow[] = [];
  let cursor: string | undefined;
  for (let guard = 0; guard < 100; guard++) {
    const page = await read(scope, cursor === undefined ? { limit } : { limit, cursor });
    rows.push(...page.rows);
    if (page.cursor === null) return rows;
    cursor = page.cursor;
  }
  throw new Error("history never ended");
};

beforeAll(async () => {
  await docker("run", "--detach", "--rm", "--name", run, "--publish", "127.0.0.1::8123", "--tmpfs", "/var/lib/clickhouse:rw",
    "--env", "CLICKHOUSE_USER=otel", "--env", `CLICKHOUSE_PASSWORD=${PASSWORD}`, "--env", "CLICKHOUSE_DB=otel", "clickhouse/clickhouse-server:25.8");
  const { stdout } = await docker("port", run, "8123/tcp");
  endpoint = { url: new URL(`http://127.0.0.1:${stdout.trim().split("\n")[0]?.split(":").at(-1)}`), user: "otel", password: Redacted.make(PASSWORD) };
  await vi.waitFor(async () => { if (!(await fetch(new URL("/ping", endpoint.url))).ok) throw new Error("not yet"); }, { timeout: 60_000, interval: 500 });
  await execFile(process.execPath, ["scripts/observe-migrate.ts"], {
    cwd: process.cwd(),
    env: { ...process.env, OBSERVE_CLICKHOUSE_URL: endpoint.url.href, CLICKHOUSE_USER: "otel", CLICKHOUSE_PASSWORD: PASSWORD },
  });
}, 240_000);

afterAll(async () => { await docker("rm", "--force", run).catch(() => undefined); }, 60_000);

const second = (n: number) => NOW_NANOS - BigInt(n) * 1_000_000_000n;

it("never returns another Organization's lines, even for the same namespace, Service and text", async () => {
  await insert([
    { org: ACME, at: second(10), body: "acme order 1 paid" },
    { org: OTHER, at: second(9), body: "other order 1 paid" },
    { org: OTHER, at: second(8), body: "other card 4242 declined" },
  ]);
  const acme = await read({});
  expect(acme.rows.map((row) => row.message)).toEqual(["acme order 1 paid"]);
  const other = await Effect.runPromise(readLogHistory(endpoint, { organizationId: OTHER, namespace: NAMESPACE }, { limit: 500, now: NOW }));
  expect(other.rows.map((row) => row.message)).toEqual(["other card 4242 declined", "other order 1 paid"]);
  expect((await read({ search: "4242" })).rows).toEqual([]);
  expect((await read({ search: "paid", machineId: "m-one" })).rows.map((row) => row.message)).toEqual(["acme order 1 paid"]);
}, 60_000);

it("pages a Service's last day newest first with no gap or repeat, through lines that share a nanosecond", async () => {
  const lines: Seed[] = [];
  for (let n = 0; n < 230; n++) {
    const at = second(1_000 + Math.floor(n / 4) * 10) + BigInt(Math.floor(n / 4));
    lines.push({ org: ACME, at, body: `page line ${n}`, container: n % 2 === 0 ? "c-one" : "c-two", machine: n % 3 === 0 ? "m-one" : "m-two" });
  }
  await insert(lines);
  const all = await pages({ search: "page line" }, 17);
  expect(all).toHaveLength(230);
  expect(new Set(all.map((row) => `${row.timestamp}/${row.hash}`)).size).toBe(230);
  expect(new Set(all.map((row) => row.message)).size).toBe(230);
  const times = all.map((row) => BigInt(row.timestamp));
  expect(times).toEqual([...times].sort((a, b) => (a < b ? 1 : a > b ? -1 : 0)));
}, 60_000);

it("searches every Machine of a Service, and narrows to one Machine on request", async () => {
  await insert([
    { org: ACME, at: second(20), body: "disk Almost Full", machine: "m-one" },
    { org: ACME, at: second(21), body: "disk almost full", machine: "m-two" },
    { org: ACME, at: second(22), body: "all quiet", machine: "m-two" },
  ]);
  const both = await read({ search: "ALMOST full" });
  expect(both.rows.map((row) => [row.machineId, row.message])).toEqual([["m-one", "disk Almost Full"], ["m-two", "disk almost full"]]);
  expect((await read({ search: "almost full", machineId: "m-two" })).rows.map((row) => row.machineId)).toEqual(["m-two"]);
}, 60_000);

it("reaches lines older than the first look-back and reports where a short page ends", async () => {
  await insert([{ org: ACME, at: NOW_NANOS - 20n * 86_400n * 1_000_000_000n, body: "three weeks ago", service: "worker", stream: "stderr" }]);
  const page = await read({ search: "three weeks ago" });
  expect(page.rows.map((row) => [row.message, row.stream, row.serviceName])).toEqual([["three weeks ago", "stderr", "worker"]]);
  expect(page.cursor).toBeNull();
}, 60_000);

it("selects one Service of a namespace by its name", async () => {
  await insert([{ org: ACME, at: second(30), body: "billing tick", service: "billing" }]);
  expect((await read({ serviceName: "billing" })).rows.map((row) => row.message)).toEqual(["billing tick"]);
  expect((await read({ serviceName: "missing" })).rows).toEqual([]);
}, 60_000);

it("pages through a line printed on both streams in one nanosecond", async () => {
  const at = second(2_000);
  await insert([
    { org: ACME, at, body: "both streams", stream: "stdout", service: "twin" },
    { org: ACME, at, body: "both streams", stream: "stderr", service: "twin" },
  ]);
  const all = await pages({ serviceName: "twin" }, 1);
  expect(all.map((row) => row.stream).sort()).toEqual(["stderr", "stdout"]);
}, 60_000);

it("searches a Windows path with backslashes as written", async () => {
  const path = String.raw`C:\temp\new\table`;
  await insert([{ org: ACME, at: second(40), body: `opened ${path}` }]);
  expect((await read({ search: path })).rows.map((row) => row.message)).toEqual([`opened ${path}`]);
}, 60_000);

it("keeps a value that looks like SQL a value", async () => {
  const hostile = "x') OR 1=1; DROP TABLE otel.otel_logs; --";
  expect((await read({ search: hostile })).rows).toEqual([]);
  expect((await read({ machineId: hostile })).rows).toEqual([]);
  expect((await read({})).rows.length).toBeGreaterThan(0);
}, 60_000);
