import "@tanstack/react-start/server-only";
import { PgClient } from "@effect/sql-pg";
import {
  type EffectPgDatabase,
  makeWithDefaults,
} from "drizzle-orm/effect-postgres";
import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { drizzle, type NodePgDatabase } from "drizzle-orm/node-postgres";
import type { PgTransactionConfig } from "drizzle-orm/pg-core";
import { Cause, Context, Data, Effect, Layer, Option, Queue, Scope, Stream } from "effect";
import * as Reactivity from "effect/unstable/reactivity/Reactivity";
import {
  isSqlError,
  isSqlErrorReason,
  SqlError,
} from "effect/unstable/sql/SqlError";
import { escapeIdentifier, Pool, type PoolConfig, type Notification } from "pg";
import { AppConfig } from "#/server/config.server";

export interface DatabaseService {
  readonly drizzle: EffectPgDatabase;
  readonly subscribe: (channel: string) => Effect.Effect<
    Stream.Stream<string, DatabaseSubscriptionFailure>,
    DatabaseSubscriptionFailure,
    Scope.Scope
  >;
  readonly afterCommit: <A, E, R>(program: Effect.Effect<A, E, R>) => Effect.Effect<void, E, R>;
  readonly transaction: <A, E, R>(
    program: Effect.Effect<A, E, R>,
    config?: PgTransactionConfig,
  ) => Effect.Effect<A, E | SqlError | DatabasePostCommitFailure, Exclude<R, Database>>;
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

export class DatabasePostCommitFailure extends Data.TaggedError(
  "DatabasePostCommitFailure",
)<{ readonly cause: unknown }> {}

export class DatabaseSubscriptionFailure extends Data.TaggedError(
  "DatabaseSubscriptionFailure",
)<{
  readonly channel: string;
  readonly operation: "connect" | "listen" | "receive";
  readonly cause: unknown;
}> {}

export function makeDatabaseService(
  drizzle: EffectPgDatabase & { $client: PgClient.PgClient },
  subscribe: DatabaseService["subscribe"],
): DatabaseService {
  const transactionService = drizzle.$client.transactionService;
  const database = transactionDatabase(drizzle);
  return database;

  function transactionDatabase(drizzle: EffectPgDatabase, pending?: Effect.Effect<void, unknown>[]): DatabaseService {
    return {
      drizzle,
      subscribe,
      afterCommit: <A, E, R>(program: Effect.Effect<A, E, R>) => Effect.gen(function* () {
        if (!pending) return yield* Effect.asVoid(program);
        const context = yield* Effect.context<R>();
        // Keep caller services, but never reuse a committed SQL connection or its savepoint depth.
        // SAFETY: SQL transaction state is implicit; declared callback capabilities remain available.
        const committedContext = Context.omit(transactionService)(context) as Context.Context<R>;
        pending.push(program.pipe(
          Effect.provide(Context.add(committedContext, Database, database)),
          Effect.asVoid,
        ));
      }),
      transaction: <A, E, R>(program: Effect.Effect<A, E, R>, config?: PgTransactionConfig) =>
        Effect.gen(function* () {
          const effects: Effect.Effect<void, unknown>[] = [];
          const result = yield* drizzle.transaction(
            (transaction) => Effect.provideService(program, Database, transactionDatabase(transaction, effects)),
            config,
          );
          if (pending) pending.push(...effects);
          else {
            yield* Effect.forEach(effects, effect => effect, { discard: true }).pipe(
              Effect.mapError(cause => new DatabasePostCommitFailure({ cause })),
            );
          }
          return result;
        }),
    };
  }
}

/** Defers work to the outermost commit, which reports DatabasePostCommitFailure.
 * Attach callback-specific recovery to program before registering it. */
export const afterDatabaseCommit = <A, E, R>(program: Effect.Effect<A, E, R>) =>
  Effect.flatMap(Database, database => database.afterCommit(program));

export function subscribeDatabaseNotifications(
  pool: Pool,
  channel: string,
): ReturnType<DatabaseService["subscribe"]> {
  return Effect.gen(function* () {
    const queue = yield* Queue.make<string, DatabaseSubscriptionFailure>();
    const client = yield* Effect.acquireRelease(
      Effect.tryPromise({
        try: () => pool.connect(),
        catch: (cause) => new DatabaseSubscriptionFailure({ channel, operation: "connect", cause }),
      }),
      (client) => Effect.sync(() => client.release(true)),
    );
    const onNotification = (message: Notification) => {
      if (message.channel === channel && message.payload !== undefined) {
        Queue.offerUnsafe(queue, message.payload);
      }
    };
    const onError = (cause: Error) => {
      Queue.failCauseUnsafe(queue, Cause.fail(
        new DatabaseSubscriptionFailure({ channel, operation: "receive", cause }),
      ));
    };
    const onEnd = () => onError(new Error("Database subscription connection ended"));
    client.on("notification", onNotification);
    client.on("error", onError);
    client.on("end", onEnd);
    yield* Effect.addFinalizer(() => Effect.sync(() => {
      client.off("notification", onNotification);
      client.off("error", onError);
      client.off("end", onEnd);
    }));
    yield* Effect.tryPromise({
      try: () => client.query(`LISTEN ${escapeIdentifier(channel)}`),
      catch: (cause) => new DatabaseSubscriptionFailure({ channel, operation: "listen", cause }),
    });
    return Stream.fromQueue(queue);
  });
}

function reportIdlePoolError(cause: Error) {
  Effect.runFork(Effect.logError("An idle PostgreSQL client failed.", cause));
}

function acquirePool(options: PoolConfig) {
  return Effect.gen(function* () {
    const pool = yield* Effect.acquireRelease(
      Effect.sync(() => {
        const pool = new Pool(options);
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
    return pool;
  });
}

function effectDatabase(pool: Pool) {
  return Effect.gen(function* () {
    const client = yield* PgClient.fromPool({
      acquire: Effect.succeed(pool),
      applicationName: "ployz-cloud",
    }).pipe(Effect.provide(Reactivity.layer));
    const applicationDatabase = yield* makeWithDefaults().pipe(
      Effect.provideService(PgClient.PgClient, client),
    );
    return makeDatabaseService(applicationDatabase, (channel) => subscribeDatabaseNotifications(pool, channel));
  });
}


/** Optional reporting cannot exhaust the pool used by ownership, cancellation and outcomes. */
export class ReportingDatabase extends Context.Service<ReportingDatabase, DatabaseService>()("ployz/ReportingDatabase") {}

export const makeReportingDatabase = (connectionString: string) => Effect.gen(function* () {
  const pool = yield* acquirePool({
    connectionString, max: 2, connectionTimeoutMillis: 200, query_timeout: 200,
    statement_timeout: 200, lock_timeout: 100,
  });
  return yield* effectDatabase(pool);
});

export const ReportingDatabaseLive = Layer.effect(ReportingDatabase, Effect.gen(function* () {
  return yield* makeReportingDatabase((yield* AppConfig).database.url.href);
}));

const makeDatabaseViews = Effect.gen(function* () {
  const config = yield* AppConfig;
  const pool = yield* acquirePool({ connectionString: config.database.url.href });
  const applicationDatabase = yield* effectDatabase(pool);
  const betterAuthDatabase = drizzle({ client: pool });

  return Context.make(Database, applicationDatabase).pipe(
    Context.add(BetterAuthDatabase, { drizzle: betterAuthDatabase }),
  );
});

export const DatabaseLive = Layer.effectContext(makeDatabaseViews);
