import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { PgClient } from "@effect/sql-pg";
import { makeWithDefaults } from "drizzle-orm/effect-postgres";
import { drizzle } from "drizzle-orm/node-postgres";
import { Effect, Exit, Layer, ManagedRuntime, Scope } from "effect";
import * as Reactivity from "effect/unstable/reactivity/Reactivity";
import { Client, Pool } from "pg";
import {
  Database,
  ReportingDatabase,
  makeDatabaseService,
  makeReportingDatabase,
} from "#/server/database.server";

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

/**
 * Shares one migrated {@link postgresTestContainer} across the promise-style tests of a file,
 * with raw `pg`, Drizzle, and Effect `Database` access over one pool.
 */
export async function startPostgresTestHarness() {
  const scope = Scope.makeUnsafe();
  const closeScope = () => Effect.runPromise(Scope.close(scope, Exit.void));
  try {
    const { url } = await Effect.runPromise(
      postgresTestContainer.pipe(
        Effect.tap(({ url }) => migrateTestDatabase(url)),
        Scope.provide(scope),
      ),
    );
    const databaseUrl = url.href;
    const pool = new Pool({ connectionString: databaseUrl, max: 8 });
    // Closed before the container is removed.
    await Effect.runPromise(
      Scope.addFinalizer(scope, Effect.promise(() => pool.end().catch(() => undefined))),
    );
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
          return makeDatabaseService(effectDatabase);
        }),
      ).pipe(
        Layer.merge(Layer.effect(ReportingDatabase, makeReportingDatabase(databaseUrl))),
        Layer.provide(Reactivity.layer),
      ),
    );
    // Finalizers run in reverse: the runtime closes before the pool.
    await Effect.runPromise(
      Scope.addFinalizer(scope, Effect.promise(() => databaseRuntime.dispose())),
    );
    const database = await databaseRuntime.runPromise(Database);
    return {
      databaseUrl,
      reportingDatabase: await databaseRuntime.runPromise(ReportingDatabase),
      db: drizzle({ client: pool }),
      database,
      pool,
      runEffect<Success, Failure>(
        operation: Effect.Effect<Success, Failure, Database | ReportingDatabase>,
      ) {
        return databaseRuntime.runPromise(operation);
      },
      runTransaction<Success, Failure>(
        operation: (
          transaction: typeof database.drizzle,
        ) => Effect.Effect<Success, Failure, Database>,
      ) {
        return databaseRuntime.runPromise(
          database.transaction(
            Effect.gen(function* () {
              return yield* operation((yield* Database).drizzle);
            }),
          ),
        );
      },
      stop: closeScope,
    };
  } catch (error) {
    await closeScope();
    throw error;
  }
}

export type PostgresTestHarness = Awaited<
  ReturnType<typeof startPostgresTestHarness>
>;
