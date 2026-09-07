import "@tanstack/react-start/server-only";
import { PgClient } from "@effect/sql-pg";
import {
  type EffectPgDatabase,
  makeWithDefaults,
} from "drizzle-orm/effect-postgres";
import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { drizzle, type NodePgDatabase } from "drizzle-orm/node-postgres";
import type { PgTransactionConfig } from "drizzle-orm/pg-core";
import { Cause, Context, Data, Effect, Layer, Option } from "effect";
import * as Reactivity from "effect/unstable/reactivity/Reactivity";
import {
  isSqlError,
  isSqlErrorReason,
  SqlError,
} from "effect/unstable/sql/SqlError";
import { Pool } from "pg";
import { AppConfig } from "#/server/config.server";

export interface DatabaseService {
  readonly drizzle: EffectPgDatabase;
  readonly transaction: <A, E, R>(
    program: Effect.Effect<A, E, R>,
    config?: PgTransactionConfig,
  ) => Effect.Effect<A, E | SqlError, Exclude<R, Database>>;
}

export class Database extends Context.Service<Database, DatabaseService>()(
  "ployz/Database",
) {}

export function sqlErrorFrom(cause: unknown): SqlError | undefined {
  const seen = new Set<unknown>();

  const walk = (cause: unknown): SqlError | undefined => {
    if (cause == null || seen.has(cause)) return undefined;
    seen.add(cause);
    if (isSqlError(cause)) return cause;
    if (isSqlErrorReason(cause)) return new SqlError({ reason: cause });
    if (cause instanceof EffectDrizzleQueryError) return walk(cause.cause);
    if (Cause.isCause(cause)) {
      const error = Cause.findErrorOption(cause);
      if (Option.isSome(error)) return walk(error.value);
    }
    return undefined;
  };

  return walk(cause);
}

export function isUniqueViolation(cause: unknown) {
  const sqlError = sqlErrorFrom(cause);
  return sqlError !== undefined && sqlError.reason._tag === "UniqueViolation";
}

export class BetterAuthDatabase extends Context.Service<
  BetterAuthDatabase,
  { readonly drizzle: NodePgDatabase }
>()("ployz/BetterAuthDatabase") {}

export class DatabasePoolCloseFailure extends Data.TaggedError(
  "DatabasePoolCloseFailure",
)<{ readonly cause: unknown }> {}

export function makeDatabaseService(
  drizzle: EffectPgDatabase,
): DatabaseService {
  return {
    drizzle,
    transaction: (program, config) =>
      drizzle.transaction(
        (transaction) =>
          Effect.provideService(
            program,
            Database,
            makeDatabaseService(transaction),
          ),
        config,
      ),
  };
}

function reportIdlePoolError(cause: Error) {
  Effect.runFork(Effect.logError("An idle PostgreSQL client failed.", cause));
}

const makeDatabaseViews = Effect.gen(function* () {
  const config = yield* AppConfig;
  const pool = yield* Effect.acquireRelease(
    Effect.sync(() => {
      const pool = new Pool({
        connectionString: config.database.url.href,
      });
      pool.on("error", reportIdlePoolError);
      return pool;
    }),
    (pool) =>
      Effect.tryPromise({
        try: () => pool.end(),
        catch: (cause) => new DatabasePoolCloseFailure({ cause }),
      }).pipe(
        Effect.ensuring(
          Effect.sync(() => pool.off("error", reportIdlePoolError)),
        ),
        Effect.orDie,
      ),
  );
  const client = yield* PgClient.fromPool({
    acquire: Effect.succeed(pool),
    applicationName: "ployz-cloud",
  }).pipe(Effect.provide(Reactivity.layer));
  const applicationDatabase = yield* makeWithDefaults().pipe(
    Effect.provideService(PgClient.PgClient, client),
  );
  const betterAuthDatabase = drizzle({ client: pool });

  return Context.make(Database, makeDatabaseService(applicationDatabase)).pipe(
    Context.add(BetterAuthDatabase, { drizzle: betterAuthDatabase }),
  );
});

export const DatabaseLive = Layer.effectContext(makeDatabaseViews);
