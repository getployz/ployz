import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import type { TestProject } from "vitest/node";

declare module "vitest" {
  export interface ProvidedContext {
    /** HTTP URL of the run's one ClickHouse server; each test file creates its own database on it. */
    clickhouseUrl: string;
  }
}

export const CLICKHOUSE_IMAGE = "clickhouse/clickhouse-server:26.9";

const execFile = promisify(execFileCallback);
const docker = (...args: ReadonlyArray<string>) => execFile("docker", args, { maxBuffer: 10 * 1024 * 1024 });

async function waitForClickHouse(port: number) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    const ready = await fetch(`http://127.0.0.1:${port}/ping`).then((response) => response.ok, () => false);
    if (ready) return;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error("ClickHouse test container did not become ready.");
}

/** Starts a throwaway ClickHouse server; `postgres.global-setup.ts` removes it if the run dies, by its `ployz-test-<pid>-` name. */
export default async function setup(project: TestProject) {
  const name = `ployz-test-${process.pid}-ch-${randomUUID().slice(0, 8)}`;
  await docker(
    "run", "--detach", "--rm", "--name", name,
    "--tmpfs", "/var/lib/clickhouse:rw",
    "--publish", "127.0.0.1::8123",
    "--env", "CLICKHOUSE_USER=ployz", "--env", "CLICKHOUSE_PASSWORD=ployz",
    "--env", "CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT=1",
    "--ulimit", "nofile=262144:262144",
    CLICKHOUSE_IMAGE,
  );
  const stop = () => docker("rm", "--force", name).then(() => undefined, () => undefined);
  try {
    const { stdout } = await docker("port", name, "8123/tcp");
    const port = Number(stdout.trim().split("\n")[0]?.split(":").at(-1));
    if (!Number.isSafeInteger(port) || port < 1) throw new Error(`Docker returned an invalid ClickHouse port: ${stdout}`);
    await waitForClickHouse(port);
    project.provide("clickhouseUrl", `http://ployz:ployz@127.0.0.1:${port}`);
    return stop;
  } catch (error) {
    await stop();
    throw error;
  }
}
