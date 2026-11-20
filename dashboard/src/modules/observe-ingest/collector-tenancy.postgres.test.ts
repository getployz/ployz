import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { afterAll, beforeAll, expect, it, vi } from "vitest";

const execFile = promisify(execFileCallback);
const docker = (...args: ReadonlyArray<string>) => execFile("docker", args, { maxBuffer: 10 * 1024 * 1024 });

const PASSWORD = "tenancy-test";
const run = `ployz-observe-test-${process.pid}-${randomUUID().slice(0, 8)}`;
const started: string[] = [];
let clickhouse = "";
let collector = "";

/** The Collector config exactly as compose ships it, with compose's `$$` escape resolved. */
function shippedCollectorConfig() {
  const compose = readFileSync(join(process.cwd(), "self-host/compose.yml"), "utf8");
  const block = /^ {2}observe-collector:\n {4}content: \|\n((?: {6}.*\n|\n)+)/m.exec(compose)?.[1];
  if (block === undefined) throw new Error("compose.yml has no observe-collector config");
  return block.replace(/^ {6}/gm, "").replaceAll("$$", "$");
}

type Row = Record<string, string | number>;

async function query(sql: string): Promise<Row[]> {
  const response = await fetch(`${clickhouse}/?default_format=JSONEachRow`, {
    method: "POST",
    headers: { authorization: `Basic ${Buffer.from(`otel:${PASSWORD}`).toString("base64")}` },
    body: sql,
  });
  const text = await response.text();
  if (!response.ok) throw new Error(text);
  return text.trim() === "" ? [] : text.trim().split("\n").map((line): Row => JSON.parse(line));
}

async function published(name: string, port: string) {
  const { stdout } = await docker("port", name, port);
  return `http://127.0.0.1:${stdout.trim().split("\n")[0]?.split(":").at(-1)}`;
}

const until = <A>(attempt: () => Promise<A | undefined>) =>
  vi.waitFor(async () => {
    const value = await attempt();
    if (value === undefined) throw new Error("not yet");
    return value;
  }, { timeout: 60_000, interval: 1000 });

const configPath = join(mkdtempSync(join(tmpdir(), "ployz-collector-")), "config.yaml");

/** One Collector from the shipped config on the shared queue volume, as compose runs it. */
async function startCollector(name: string) {
  await docker("run", "--detach", "--rm", "--name", name, "--network", run, "--publish", "127.0.0.1::4318",
    "--env", `CLICKHOUSE_PASSWORD=${PASSWORD}`, "--volume", `${configPath}:/etc/otelcol/config.yaml:ro`, "--volume", `${run}-queue:/queue`,
    "otel/opentelemetry-collector-contrib:0.147.0", "--config=/etc/otelcol/config.yaml");
  started.push(name);
  collector = await published(name, "4318/tcp");
  await until(async () => ((await fetch(`${collector}/v1/logs`, { method: "POST" })).status > 0 ? true : undefined));
}

beforeAll(async () => {
  await docker("network", "create", run);
  started.push(`network:${run}`);
  await docker("run", "--detach", "--rm", "--name", `${run}-ch`, "--network", run, "--network-alias", "clickhouse", "--publish", "127.0.0.1::8123",
    "--tmpfs", "/var/lib/clickhouse:rw", "--env", "CLICKHOUSE_USER=otel", "--env", `CLICKHOUSE_PASSWORD=${PASSWORD}`, "--env", "CLICKHOUSE_DB=otel",
    "clickhouse/clickhouse-server:25.8");
  started.push(`${run}-ch`);
  clickhouse = await published(`${run}-ch`, "8123/tcp");
  await until(async () => ((await fetch(`${clickhouse}/ping`)).ok ? true : undefined));
  await execFile(process.execPath, ["scripts/observe-migrate.ts"], {
    cwd: process.cwd(),
    env: { ...process.env, OBSERVE_CLICKHOUSE_URL: clickhouse, CLICKHOUSE_USER: "otel", CLICKHOUSE_PASSWORD: PASSWORD },
  });
  writeFileSync(configPath, shippedCollectorConfig());
  await docker("volume", "create", `${run}-queue`);
  started.push(`volume:${run}-queue`);
  await docker("run", "--rm", "--user", "0", "--volume", `${run}-queue:/queue`, "busybox:1.37", "chown", "10001:10001", "/queue");
  await startCollector(`${run}-col`);
}, 240_000);

afterAll(async () => {
  for (const name of started.reverse()) {
    const remove = name.startsWith("network:") ? docker("network", "rm", name.slice(8))
      : name.startsWith("volume:") ? docker("volume", "rm", name.slice(7))
      : docker("rm", "--force", name);
    await remove.catch(() => undefined);
  }
}, 60_000);

const repeated = (key: string, value: string) => [1, 2, 3].map(() => ({ key, value: { stringValue: value } }));

const forged = (body: string) => JSON.stringify({
  resourceLogs: [{
    resource: { attributes: [
      { key: "service.name", value: { stringValue: "web" } },
      ...repeated("ployz.org", "victim-org"),
      ...repeated("ployz.retention_days", "9999"),
    ] },
    scopeLogs: [{ logRecords: [{
      timeUnixNano: `${Date.now()}000000`,
      body: { stringValue: body },
      attributes: repeated("ployz.org", "victim-org"),
    }] }],
  }],
});

async function post(body: string, headers: Record<string, string>) {
  const response = await fetch(`${collector}/v1/logs`, { method: "POST", headers: { "content-type": "application/json", ...headers }, body });
  expect(response.status).toBe(200);
}

it("files a record under the org Cloud stamped, never the one the sender claimed", async () => {
  await post(forged("unstamped"), {});
  await post(forged("stamped"), { "x-ployz-org": "real-org", "x-ployz-retention-days": "9999" });
  const rows = await until(async () => {
    const found = await query("SELECT Body, OrgId, RetentionDays FROM otel.otel_logs ORDER BY Body");
    return found.length === 2 ? found : undefined;
  });
  expect(rows).toEqual([
    { Body: "stamped", OrgId: "real-org", RetentionDays: 400 },
    { Body: "unstamped", OrgId: "", RetentionDays: 30 },
  ]);
  expect(await query("SELECT count() AS n FROM otel.otel_logs WHERE OrgId = 'victim-org'")).toEqual([{ n: 0 }]);
  expect(await query("SELECT count() AS n FROM otel.otel_logs WHERE has(mapValues(ResourceAttributes), 'victim-org') OR has(mapValues(LogAttributes), 'victim-org')")).toEqual([{ n: 0 }]);
}, 120_000);

it("keeps a batch it answered 200 through a ClickHouse outage and a Collector crash", async () => {
  await docker("pause", `${run}-ch`);
  await post(forged("acked-during-outage"), { "x-ployz-org": "real-org", "x-ployz-retention-days": "30" });
  await docker("kill", `${run}-col`);
  await startCollector(`${run}-col-again`);
  await docker("unpause", `${run}-ch`);
  const rows = await vi.waitFor(async () => {
    const found = await query("SELECT OrgId FROM otel.otel_logs WHERE Body = 'acked-during-outage'");
    if (found.length === 0) throw new Error("not yet");
    return found;
  }, { timeout: 120_000, interval: 2000 });
  expect(rows).toEqual([{ OrgId: "real-org" }]);
}, 240_000);
