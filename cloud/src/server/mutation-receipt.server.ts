import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import type { PgTransactionConfig } from "drizzle-orm/pg-core";
import { Data, Effect } from "effect";
import { Database, sqlErrorFrom } from "#/server/database.server";

export interface MutationReceipt<A> {
  readonly data: A;
  readonly txid: number;
}

export class InvalidTransactionReceipt extends Data.TaggedError(
  "InvalidTransactionReceipt",
)<{ readonly value: unknown }> {}

const currentTransactionId = Effect.fn("Database.currentTransactionId")(
  function* () {
    const database = yield* Database;
    const rows = yield* database.drizzle.execute<{ txid: string }>(
      sql`select pg_current_xact_id()::xid::text as txid`,
      "objects",
    );
    const value = rows[0]?.txid;
    const txid = value === undefined ? Number.NaN : Number(value);
    if (!Number.isSafeInteger(txid)) {
      return yield* new InvalidTransactionReceipt({ value });
    }
    return txid;
  },
);

export const mutationReceiptInTransaction = Effect.fn(
  "Database.mutationReceiptInTransaction",
)(function* <A>(data: A) {
  return { data, txid: yield* currentTransactionId() } satisfies MutationReceipt<A>;
});

function isSerializationFailure(cause: unknown) {
  const sqlError = sqlErrorFrom(cause);
  return sqlError !== undefined && sqlError.reason._tag === "SerializationError";
}

export const withMutationReceipt = Effect.fn("Database.withMutationReceipt")(
  function* <A, E, R>(
    program: Effect.Effect<A, E, R | Database>,
    options?: {
      readonly isolationLevel?: PgTransactionConfig["isolationLevel"];
    },
  ) {
    const database = yield* Database;
    const transact = () =>
      database.transaction(
        Effect.gen(function* () {
          const data = yield* program;
          const txid = yield* currentTransactionId();
          return { data, txid } satisfies MutationReceipt<A>;
        }),
        options?.isolationLevel === undefined
          ? undefined
          : { isolationLevel: options.isolationLevel },
      );
    return yield* transact().pipe(
      Effect.retry({ times: 1, while: isSerializationFailure }),
      Effect.annotateLogs({
        isolationLevel: options?.isolationLevel ?? "default",
      }),
    );
  },
);
