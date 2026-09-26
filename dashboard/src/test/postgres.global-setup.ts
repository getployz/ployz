import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { Client } from "pg";
import type { TestProject } from "vitest/node";

declare module "vitest" {
  export interface ProvidedContext {
    /** Maintenance URL of the run's one PostgreSQL server; its `ployz_cloud` database is migrated. */
    postgresAdminUrl: string;
  }
}

const execFile = promisify(execFileCallback);

async function docker(...args: ReadonlyArray<string>) {
  return execFile("docker", args, { maxBuffer: 10 * 1024 * 1024 });
}

async function waitForPostgres(url: URL) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const client = new Client({ connectionString: url.href });
    try {
      await client.connect();
      await client.query("select 1");
      await client.end();
      return;
    } catch {
      await client.end().catch(() => undefined);
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }
  throw new Error("PostgreSQL test container did not become ready.");
}

/** Starts a throwaway PostgreSQL server with a migrated `ployz_cloud` database. */
export async function startPostgresServer() {
  const name = `ployz-test-${process.pid}-${randomUUID().slice(0, 8)}`;
  await docker(
    "run",
    "--detach",
    "--rm",
    "--name",
    name,
    "--tmpfs",
    "/var/lib/postgresql/data:rw",
    "--publish",
    "127.0.0.1::5432",
    "--env",
    "POSTGRES_PASSWORD=postgres",
    "--env",
    "POSTGRES_DB=ployz_cloud",
    "postgres:16-alpine",
    "-c",
    "fsync=off",
    "-c",
    "synchronous_commit=off",
    "-c",
    "full_page_writes=off",
    "-c",
    "max_connections=500",
  );
  const stop = () => docker("rm", "--force", name).then(() => undefined, () => undefined);
  try {
    const { stdout } = await docker("port", name, "5432/tcp");
    const port = Number(stdout.trim().split(":").at(-1));
    if (!Number.isSafeInteger(port) || port < 1) {
      throw new Error(`Docker returned an invalid PostgreSQL port: ${stdout}`);
    }
    const url = new URL(`postgres://postgres:postgres@127.0.0.1:${port}/ployz_cloud`);
    // The image's init server listens only on its socket, so the mapped port is ready only once
    // the real server is up.
    await waitForPostgres(url);
    await execFile(process.execPath, ["node_modules/drizzle-kit/bin.cjs", "migrate"], {
      cwd: process.cwd(),
      env: { ...process.env, DATABASE_URL: url.href },
      maxBuffer: 20 * 1024 * 1024,
    });
    return { url, stop };
  } catch (error) {
    await stop();
    throw error;
  }
}

/**
 * Starts one server for the whole run; each test database is a copy of its migrated
 * `ployz_cloud` (see `postgresTestDatabase`).
 */
export default async function setup(project: TestProject) {
  const { url, stop } = await startPostgresServer();
  url.pathname = "/postgres";
  project.provide("postgresAdminUrl", url.href);
  return stop;
}
