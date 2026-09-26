import { randomUUID } from "node:crypto";
import { PgClient } from "@effect/sql-pg";
import { makeWithDefaults } from "drizzle-orm/effect-postgres";
import { drizzle } from "drizzle-orm/node-postgres";
import { Effect, Exit, Layer, ManagedRuntime, Scope } from "effect";
import * as Reactivity from "effect/unstable/reactivity/Reactivity";
import { Client, Pool } from "pg";
import { inject } from "vitest";
import {
  Database,
  ReportingDatabase,
  makeDatabaseService,
  makeReportingDatabase,
} from "#/server/database.server";
import { startPostgresServer } from "#/test/postgres.global-setup";

async function withAdminClient<A>(use: (client: Client) => Promise<A>) {
  const client = new Client({ connectionString: inject("postgresAdminUrl") });
  await client.connect();
  try {
    return await use(client);
  } finally {
    await client.end();
  }
}

/**
 * A fresh, migrated database on the run's PostgreSQL server (see `postgres.global-setup.ts`),
 * copied from the migrated `ployz_cloud` template and dropped on release.
 */
export const postgresTestDatabase = Effect.acquireRelease(
  Effect.promise(async () => {
    const name = `test_${randomUUID().replaceAll("-", "")}`;
    await withAdminClient((client) =>
      client.query(`create database ${name} template ployz_cloud`),
    );
    const url = new URL(inject("postgresAdminUrl"));
    url.pathname = `/${name}`;
    return { name, url };
  }),
  ({ name }) =>
    Effect.promise(() =>
      withAdminClient((client) =>
        client.query(`drop database if exists ${name} with (force)`),
      ),
    ).pipe(Effect.ignore),
);

/**
 * Shares one {@link postgresTestDatabase} across the promise-style tests of a file,
 * with raw `pg`, Drizzle, and Effect `Database` access over one pool.
 *
 * `ownServer` starts a dedicated server instead: the change log's xid horizon is cluster-wide,
 * so other files' open transactions would hold back its reads.
 */
export async function startPostgresTestHarness({ ownServer = false } = {}) {
  const scope = Scope.makeUnsafe();
  const closeScope = () => Effect.runPromise(Scope.close(scope, Exit.void));
  try {
    const testDatabase: Effect.Effect<{ url: URL }, never, Scope.Scope> = ownServer
      ? Effect.acquireRelease(Effect.promise(startPostgresServer), ({ stop }) => Effect.promise(stop))
      : postgresTestDatabase;
    const { url } = await Effect.runPromise(testDatabase.pipe(Scope.provide(scope)));
    const databaseUrl = url.href;
    const pool = new Pool({ connectionString: databaseUrl, max: 8 });
    // Closed before the database is dropped.
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
