import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { drizzle } from "drizzle-orm/node-postgres";
import { PgClient } from "@effect/sql-pg";
import { makeWithDefaults } from "drizzle-orm/effect-postgres";
import { Effect, Layer, ManagedRuntime, Result } from "effect";
import * as Reactivity from "effect/unstable/reactivity/Reactivity";
import { Pool } from "pg";
import {
  Database,
  makeDatabaseService,
  subscribeDatabaseNotifications,
} from "#/server/database.server";
import {
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  createGitServiceSource,
  projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";

const execFile = promisify(execFileCallback);

export function savedGithubServiceNode(input: {
  serviceId: string;
  lineageId: string;
  installationId?: number;
  repositoryId?: number;
}) {
  return {
    nodeType: "service" as const,
    nodeId: input.serviceId,
    nodeLineageId: input.lineageId,
    configVersion: 1,
    config: projectServiceDeploymentConfig({
      name: "API",
      source: createGitServiceSource({
        repository: "acme/api",
        installationId: input.installationId ?? 17,
        repositoryId: input.repositoryId ?? 42,
      }),
      preDeployCommand: null,
      startCommand: null,
      healthcheck: createDefaultServiceHealthcheck(),
      restartPolicy: createDefaultServiceRestartPolicy(),
      privateDns: "api",
      build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
    }),
    encryptedRegistryUsername: null,
    encryptedRegistrySecret: null,
  };
}

async function docker(...args: string[]) {
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
  throw new Error("PostgreSQL 16 acceptance container did not become ready.");
}

async function waitForMappedPostgres(pool: Pool) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      await pool.query("select 1");
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
  }
  throw new Error("Mapped PostgreSQL acceptance port did not become ready.");
}

export async function startGithubPostgresTestHarness() {
  const containerName = `ployz-github-${process.pid}-${randomUUID().slice(0, 8)}`;
  await docker(
    "run",
    "--detach",
    "--rm",
    "--name",
    containerName,
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
    await waitForPostgres(containerName);
    const { stdout } = await docker("port", containerName, "5432/tcp");
    const port = Number(stdout.trim().split(":").at(-1));
    if (!Number.isSafeInteger(port) || port < 1) {
      throw new Error(`Docker returned an invalid PostgreSQL port: ${stdout}`);
    }
    const databaseUrl = `postgres://postgres:postgres@127.0.0.1:${port}/ployz_cloud`;
    const pool = new Pool({ connectionString: databaseUrl, max: 8 });
    await waitForMappedPostgres(pool);
    await execFile(process.execPath, [
      "--env-file-if-exists=.env",
      "node_modules/drizzle-kit/bin.cjs",
      "migrate",
    ], {
      cwd: process.cwd(),
      env: { ...process.env, DATABASE_URL: databaseUrl },
      maxBuffer: 20 * 1024 * 1024,
    });
    const db = drizzle({ client: pool });
    const databaseRuntime = ManagedRuntime.make(
      Layer.effect(
        Database,
        Effect.gen(function* () {
          const client = yield* PgClient.fromPool({
            acquire: Effect.acquireRelease(
              Effect.succeed(pool),
              () => Effect.void,
            ),
            applicationName: "ployz-cloud-test",
          });
          const effectDatabase = yield* makeWithDefaults().pipe(
            Effect.provideService(PgClient.PgClient, client),
          );
          return makeDatabaseService(effectDatabase, (channel) => subscribeDatabaseNotifications(pool, channel));
        }),
      ).pipe(Layer.provide(Reactivity.layer)),
    );
    const database = await databaseRuntime.runPromise(Database);
    return {
      databaseUrl,
      db,
      database,
      runEffect<Success, Failure>(
        operation: Effect.Effect<Success, Failure, Database>,
      ) {
        return databaseRuntime.runPromise(operation);
      },
      runTransactionResult<Success, Failure>(
        operation: (
          transaction: typeof database.drizzle,
        ) => Effect.Effect<Success, Failure, Database>,
      ) {
        return databaseRuntime.runPromise(
          Effect.result(
            database.transaction(
              Effect.gen(function* () {
                const transaction = (yield* Database).drizzle;
                return yield* operation(transaction);
              }),
            ),
          ),
        );
      },
      runTransaction<Success, Failure>(
        operation: (
          transaction: typeof database.drizzle,
        ) => Effect.Effect<Success, Failure, Database>,
      ) {
        return databaseRuntime.runPromise(
          database.transaction(
            Effect.gen(function* () {
              const transaction = (yield* Database).drizzle;
              return yield* operation(transaction);
            }),
          ),
        );
      },
      pool,
      async stop() {
        await databaseRuntime.dispose();
        await pool.end();
        await docker("rm", "--force", containerName).catch(() => undefined);
      },
    };
  } catch (error) {
    await docker("rm", "--force", containerName).catch(() => undefined);
    throw error;
  }
}

export type GithubPostgresTestHarness = Awaited<
  ReturnType<typeof startGithubPostgresTestHarness>
>;

export function runGithubRepositoryResult<Success, Failure>(
  harness: GithubPostgresTestHarness,
  operation: Effect.Effect<Success, Failure, Database>,
): Promise<Result.Result<Success, Failure>> {
  return harness.runEffect(Effect.result(operation));
}
