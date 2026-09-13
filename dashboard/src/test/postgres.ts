import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { Effect } from "effect";
import { Client } from "pg";

const execFile = promisify(execFileCallback);

async function docker(...args: ReadonlyArray<string>) {
  return execFile("docker", args, { maxBuffer: 10 * 1024 * 1024 });
}

async function waitForPostgres(containerName: string) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      await docker(
        "exec",
        containerName,
        "pg_isready",
        "-U",
        "postgres",
        "-d",
        "ployz_cloud",
      );
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }
  throw new Error("PostgreSQL test container did not become ready.");
}

async function waitForMappedPostgres(url: URL) {
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
  throw new Error("Mapped PostgreSQL test port did not become ready.");
}

export const postgresTestContainer = Effect.acquireRelease(
  Effect.promise(async () => {
    const name = `ployz-effect-${process.pid}-${randomUUID().slice(0, 8)}`;
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
    );
    try {
      await waitForPostgres(name);
      const { stdout } = await docker("port", name, "5432/tcp");
      const port = Number(stdout.trim().split(":").at(-1));
      if (!Number.isSafeInteger(port) || port < 1) {
        throw new Error(`Docker returned an invalid PostgreSQL port: ${stdout}`);
      }
      const url = new URL(
        `postgres://postgres:postgres@127.0.0.1:${port}/ployz_cloud`,
      );
      await waitForMappedPostgres(url);
      return {
        name,
        url,
      };
    } catch (error) {
      await docker("rm", "--force", name).catch(() => undefined);
      throw error;
    }
  }),
  ({ name }) =>
    Effect.promise(() => docker("rm", "--force", name)).pipe(
      Effect.ignore,
    ),
);

export const migrateTestDatabase = (url: URL) =>
  Effect.promise(() =>
    execFile(process.execPath, [
      "--env-file-if-exists=.env",
      "node_modules/drizzle-kit/bin.cjs",
      "migrate",
    ], {
      cwd: process.cwd(),
      env: { ...process.env, DATABASE_URL: url.href },
      maxBuffer: 20 * 1024 * 1024,
    }),
  );
